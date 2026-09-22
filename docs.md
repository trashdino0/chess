# Ferrum — How It Works

Ferrum (`chess-engine`) is a classical chess engine in zero-dependency Rust:
it picks moves by **searching** future positions with alpha-beta negamax and
scoring them with a **hand-written evaluation**. No neural network, no opening
book, no tablebases — just game-tree search plus chess knowledge encoded as
numbers. This document explains every part, aimed at learning. For the
short parameter tables see `MODEL.md`; for contributor rules see `AGENTS.md`;
for what to build next see `plan.md`.

Notation used below: scores are **centipawns** (100 = one pawn),
`depth`/`ply` count half-moves, and `evaluate()` always sees the board from
**White's perspective** (positive = good for White).

---

## 0. The 30-second tour: how a move gets chosen

1. The GUI sends `position startpos moves e2e4 e7e5` + `go wtime 600000 btime 600000`.
2. `search_root` generates all legal moves, then searches each one deeper and
   deeper (**iterative deepening**: depth 1, 2, 3, … until time runs out).
3. Each move is evaluated by `negamax`: recursively explore replies, assume
   both sides play their best, cut off branches that can't matter
   (**alpha-beta pruning**), and score quiet endpoints with `evaluate()`
   (or `quiescence()` near the leaves to avoid tactical blindness).
4. Scores are cached in the **transposition table** (TT) so repeated positions
   are looked up, not re-searched; **move ordering** (TT move → captures →
   killers → history) makes pruning cut earlier.
5. When time expires, the best move found at the deepest *completed* depth is
   reported: `bestmove g1f3`. An aborted half-finished depth is thrown away —
   only finished work counts.

Everything else in this file is one of those steps in detail.

---

## 1. Board representation (`src/board.rs`, `src/types.rs`)

**Squares** are numbers `0 = a1` through `63 = h8`
(`sq = rank * 8 + file`). `file_of` / `rank_of` / `make_sq` convert.

**Pieces** live in bitboards — one 64-bit integer per piece type per color:

```rust
pieces: [[u64; 6]; 2]   // [color][piece] -> occupancy bitboard
occ: [u64; 2]           // per-color occupancy
occ_all: u64            // both sides
```

There is no mailbox array. `piece_at(sq)` checks occupancy, then scans the
six type bitboards — this is the single "what's on this square?" primitive
used by movegen, eval, and search. Bit operations used throughout:
`bit(sq)` makes a single-bit mask, `pop_lsb(&mut bb)` removes and returns the
lowest set square (used to loop over pieces without allocating).

**State** beside the pieces: side to move, castling rights (bits
`WK=1, WQ=2, BK=4, BQ=8`), an optional en-passant square, halfmove clock,
fullmove number, and the Zobrist hash.

**Zobrist hashing** identifies positions for repetition detection and the TT.
Each (piece, square), castling-rights, and EP-file combination owns a random
64-bit key from a deterministic `splitmix64` stream (seed `0x243F6A8885A308D3`,
so every build hashes identically). A position's hash is those keys XORed
together. Note: `make_move` currently **recomputes** the hash from scratch
(`compute_hash`) rather than updating it incrementally — simple, and a known
future speedup (see `plan.md`).

**Undo** (`Undo` struct) snapshots `move`, `captured` (+ its square),
castling rights, EP, halfmove clock, and hash before each move, so
`unmake_move` restores the exact prior state. Invariant: captures are
cleared/restored by **recorded owner** (`cap.color`), never by assuming the
enemy — this is what keeps make/unmake symmetric (see §2's EP lesson).

**Draw tracking**: every position hash (including initial and after null
moves) is pushed to `hash_history` and counted in `hash_counts`, so
repetition checks are O(1). Fifty-move clock and insufficient-material
(kings + at most one minor each side) complete the draw rules.

---

## 2. Move generation (`src/movegen.rs`, `src/bitboard.rs`)

A `Move` is only `{from, to, promotion}`. Castling, en passant, and double
pushes are **inferred** from board state inside `make_move`, never stored:

- King moving two files sideways also relocates the rook.
- A pawn capturing onto the EP square while the target is empty removes the
  pawn on the square behind (`from ∓ 8`).
- A pawn double push sets the EP square between start and end.

Generation is two stages: `generate_pseudo()` emits pseudo-legal moves
(type-major order: pawns/knights via lookup tables, sliders via ray attacks
along ranks/files/diagonals), then `generate_legal()` keeps only those that
leave the mover's king safe (make → `in_check` → unmake). Quiescence search
uses a cheaper path: `generate_pseudo_captures()` emits captures, EP, and
all promotions only, legality-checked by `generate_captures()`. Eval reuses
the same attack logic allocation-free through `mobility_count()`.

**EP rule that matters.** An en-passant capture is generated only if the
target *is* the EP square **and** an enemy pawn actually stands on the jumped
square (`ep_capture_ok`). Without the victim check, stale EP squares in FENs
emit *phantom* captures (e.g. Black `c7xd6`/`e7xd6` with no white pawn on
d5). Lesson learned the hard way: those phantoms corrupted make/unmake (the
"captured" own pawn was restored to the wrong color, materializing a white
pawn on d7), the poisoned board searched a fantasy position, and the engine
emitted king captures (`d7xe8=Q`) — 13 lost GUI games from one missing check.
Regression test: `stale_ep_square_emits_no_phantom_capture`.

**Proof of correctness: perft.** `perft(depth)` counts all leaf nodes and is
checked against published reference counts (startpos d1–4:
20/400/8902/197281; Kiwipete; position 3; endgame). Perft touches only
movegen + make/unmake — never search or eval — so it isolates exactly this
chapter. Any movegen change must keep perft exact.

---

## 3. Evaluation (`src/eval.rs`)

`evaluate(board) -> i32` scores White-minus-Black in centipawns. Every term
is computed **owner-perspective and signed once by color** — the invariant
that makes color-mirrored positions negate exactly (a past bug credited
Black's development bonus to White and hid behind symmetric tests).
`side_eval()` in search flips the score for Black to move (negamax needs
side-relative scores).

**Game phase** (0 = bare kings, 24 = full material) weights pieces
N/B = 1, R = 2, Q = 4 (pawns/kings = 0), clamped to `[0, 24]`. It drives all
middlegame→endgame interpolation with the blend
`(mg * phase + eg * (24 - phase)) / 24`.

**Piece-square tables** (rank8-first arrays, read from the owner's
perspective): `PAWN`, `KNIGHT`, `BISHOP`, `ROOK`, `QUEEN`, `KING_MG`, each
with an `_EG` endgame mate (`pawn_pst` … `king_pst`). Kings shelter early
and centralize late; endgame pawns reward advanced central pawns; rooks and
queens mildly centralize.

**Structure and piece terms**, all owner-signed:

| Term | Value |
|---|---|
| Passed pawn (by rank) | `PASSED_BONUS = [0,0,10,20,35,60,100,0]` |
| Passer king distance ("the square," endgame-weighted) | `(d_enemy − d_own) × 6 × (24−phase)/24`, Chebyshev distance to promotion |
| Isolated pawn / doubled pawn | −12 / −14 |
| Rook open / semi-open file | +18 / +10 |
| Rook on 7th rank | +20 |
| Mobility per pseudo-legal destination (N/B/R/Q) | +2/+2/+2/+1 |
| Developed minor / central minor | +6 / +4 |
| Pawn shield (home king, 3 files × +8 one rank ahead / +4 two ahead) | middlegame-weighted |
| Bishop pair | +30 |
| Tempo (side-to-move bonus) | `TEMPO = 12` |

Tempo lives in `evaluate` (not `side_eval`), so the `eval` command shows it
and the mover is always credited. Consequence: symmetric positions differ
only by tempo, and **startpos evaluates to exactly 12, not 0** — tests pin
this (`startpos_is_balanced`, `eval_is_color_symmetric` with mirrored side).

---

## 4. Search (`src/search.rs`)

**Negamax with alpha-beta.** `negamax(board, depth, alpha, beta)` returns the
side-to-move-relative score. Alpha = best the mover is guaranteed; beta =
best the opponent will allow. Any line proving `score ≥ beta` is cut
(opponent would never permit it); on success the value is exact or bounded
and stored in the TT with a flag (`EXACT`/`LOWER`/`UPPER`).

**Iterative deepening + aspiration.** The root searches depth 1, 2, 3, …
until time runs out; each depth reuses ordering from the last. From depth 4,
each iteration starts in a narrow window (`prev ± 25`) and widens
(doubling) on fail-high/low — most depths confirm cheaply, outliers pay for
a re-search. Aspiration is skipped once a mate score appears.

**Root (PVS).** The first move gets a full window; the rest get a
zero-width "prove you're better" window and only a full re-search if they
look better. Each completed iteration is stored to the TT (this is what
`collect_pv` walks for the `pv` output) and published for SMP. No legal
moves → `bestmove 0000` (mate scores `-MATE` if in check, else stalemate 0).

**Transposition table.** Power-of-two array of lock-free slots
(`key`/`data`/`check` as atomics, `check = key ^ data ^ SALT` so torn
cross-thread reads fail closed as misses). Each entry packs 63 bits:
16-bit movecode (`0` = none, else `1 + (from | to≪6 | promo≪12)`),
21-bit score offset by 2²⁰, 8-bit depth, 2-bit flag, 16-bit generation.
Replacement is generation-aware: a fresh generation (bumped by
`new_search()` at every `search_root`) supersedes stale same-key entries
even when shallower; within one generation the deeper entry wins.
**Mate scores are adjusted by ply** (store adds, probe
subtracts) so "mate in 5" means the same thing at every depth. Startup size
is the `Hash` option in MB (default 16).

**Move ordering** (best first = more cutoffs = exponentially less work):
hash move (10M) → promotions (900k + victim) → winning captures by MVV-LVA
(`1M + victim×10 − attacker`, SEE ≥ 0) → EP → killers (800k/790k) →
countermove (785k: the reply that worked last time after the previous move)
→ losing captures (`700k + SEE`, below counter) → history score. The SEE
split demotes sacrifices like defended `QxP` (−800) to the back instead of
fronting them on victim value. History gives `+depth²` (clamped +1M) to cutoff quiets and symmetric
**gravity** `−depth²` (clamped −1M) to quiet fail-lows, in root and negamax —
measured −4.9% bench nodes.

**Pruning and reductions** (all verified by perft-exactness + gates, since
each one risks cutting something real):

| Technique | Rule |
|---|---|
| Null-move pruning | depth ≥ 3, not in check, non-pawn material, static ≥ beta → search at `depth−1−(2 + (depth>6))` with a null window; cutoff on `≥ beta` (never on mate scores) |
| Internal iterative reduction | depth ≥ 6 with no hash move → search one ply shallower to seed the TT |
| Check extension | side to move in check → +1 depth (bounded by `MAX_PLY = 64`) |
| Futility pruning | depth ≤ 2, not in check, no mate scores near → `static + margin ≤ alpha` (120/250) drops to quiescence |
| Late-move pruning | depth ≤ 2, late quiet non-killers (`i ≥ 6 + depth×6`) skipped near leaves |
| Late-move reduction | depth ≥ 3, late quiet non-killers reduced 1 ply (2 if depth ≥ 6 and very late), full re-search if it beats alpha |
| Mate distance pruning | alpha/beta clamped to `∓MATE ± ply` |

**Quiescence.** Instead of a raw eval at the horizon (which is blind to
hanging pieces), search continues with captures and promotions only — plus
full evasions when in check. **Delta pruning** skips captures that can't
recover alpha even winning the full exchange (`SEE + static + 150 ≤ alpha`;
promotions never skipped, evasions exempt). Depth-capped at
`MAX_PLY + 24`, then static eval. Draws (fifty-move,
insufficient material, repetition) return 0 inside search but never touch
perft.

**Static exchange evaluation** (`see()` in `movegen.rs`): net material for
the side to move if a capture is played and both sides recapture with
least-valuable attackers. Placement snapshot + occupancy are updated per
swap so X-rays join once uncovered (taking through to a backed piece
scores the recapture); either side stops when stopping scores better
(minimax over stopping points). Pins ignored (standard). Cost: ~23% nps;
payoff: bench8 2.91M → 0.78M nodes and steadier scores with depth.

**Lazy SMP (`Threads` option).** Helper threads join via `thread::scope`,
each with an owned board clone, the shared TT handle, staggered
`start_depth = min(1+tid, max)`, private killers/history/stack, and its own
deadline — sharing only stop flag, best-result slot, and node counters.
Helpers are silent; the main thread prints combined nodes/nps. Measured:
startpos `movetime 5000`, 1 thread = depth 13 @893k nps vs 4 threads =
depth 15 @3.19M nps (+2 ply). `bench`/`search` CLI stay single-threaded for
deterministic numbers.

**Abort and time.** A poll callback (checked every 1024 nodes) aborts on
`stop`/`quit`/disconnect/deadline by unwinding with `Err(Abort)`; the root
keeps the previous finished iteration. Time per move: `movetime − 30ms`, or
clock-based `own / min(movestogo, 50)` (default divisor 20) `+ inc/2`,
clamped. Known thin margins under load — see `plan.md`.

---

## 5. Protocols and tools

**UCI** (`src/uci.rs`): a stdin-reader thread forwards lines over a channel;
`go` runs on the main thread while `poll()` drains `stop`/`quit` (mid-search
`isready` is deferred, `position`/`ucinewgame` queued). Commands: `uci`,
`isready`, `setoption` (`Hash`, `Threads`), `ucinewgame`, `position`
(fen/startpos + validated move list — stops at the first illegal move),
`go` (depth/movetime/clocks; bare `go` = depth 6), `stop`, `quit`,
plus debug commands `d` (board), `eval`, `perft`, `bench`.
**Testing gotcha:** piped stdin always yields depth-1-only searches, because
EOF sets the abort flag — correct engine behavior, not a bug. Drive full
sessions interactively or via GUI.

**CLI shortcuts** (deterministic, single-threaded — preferred for testing):
`perft <depth> [fen]`, `search <depth> [fen]`, `bench [depth]`
(10-FEN suite, currently 2,909,536 nodes at depth 8).

---

## 6. Testing and measuring strength

- `cargo test` (34 tests): perft suites, eval pins, TT pack/probe/threads,
  make/unmake round-trips incl. the EP regression, repetition counting.
- **Bench discipline:** compare back-to-back binaries (nps noise is ±8%);
  fixed-depth node/bestmove wobble is TT-luck, so judge ordering by trends
  and strength by games, never by single fixed-depth numbers. The shared TT
  pollutes later bench positions — use fresh-TT `search` for single-position
  truth.
- **Self-play gates** (`cutechess-cli`, `benchmarks/openings.epd`): 60 games
  ≈ ±90 Elo (catches crashes/disasters), 200 games ≈ ±50 (keep/revert
  calls). Always read terminations — illegal moves, disconnects, and
  forfeits say more than the score. History so far: tapered eval kept,
  history gravity kept at 32–19–9, and one 200-game attempt famously exposed
  the phantom-EP bug instead.
- **Ground truth:** no absolute rating yet. Candidates: a TSCP anchor gate
  (≈1800) or the lichess BOT account playing rated rapid/classical — see
  `plan.md`. Rough human-pool estimate today: ~1700–2100 depending on time
  control (tactically sharp, positionally naive, no book).

---

## 7. Known limitations (see `plan.md` for the fix list)

Zero crates by design (no SIMD, no NNUE-style eval); full recompute of the
Zobrist hash per make; `generate_legal` allocates a vector per node; TT has
no generation aging; no SEE yet; no opening book or tablebases in-engine
(the lichess bridge can supply books externally); time margins are thin for
bullet; `openings.epd` contains one synthetic illegal-ish French line with a
stale EP square (kept deliberately as phantom-EP coverage).

---

## 8. Future direction: NNUE evaluation (researched, see `plan.md` P9)

The hand-written eval in §3 is slated for a neural replacement that keeps
the zero-dependency design: training runs **outside** the repo with bullet
(the Rust NNUE trainer most top engines use); the engine gains only scalar
quantized inference plus weights embedded via `include_bytes!` — no crates.
The starting architecture is HalfKP[40960]→256x2→32→32→1 (~10M parameters,
~20MB quantized): each feature is an (own-king-square, piece, square) tuple,
and only the handful of features a move touches are re-added to running
accumulators (2 for quiet moves, 3–4 for captures/castling) instead of
recomputing the network from scratch — the same "update, don't recompute"
idea as incremental Zobrist hashing. Training needs 100M+ quiet labeled
positions (self-play at fixed nodes after random plies; our lichess PGNs
seed variety but get relabeled by our own search). First-net expectation is
roughly +80–150 Elo over the HCE. Hardest correctness risk is accumulator
desync in make/unmake — same silent-corruption class as the phantom-EP bug,
gated by exact incremental-vs-full-recompute tests.

---

## Glossary

- **Ply** — one half-move (White's move + Black's reply = 2 ply). Search
  depth is measured in ply.
- **PV (principal variation)** — the predicted best line; printed after
  `pv` in `info` output, collected by walking TT best moves.
- **TT (transposition table)** — hash cache of searched positions (§4).
- **Quiescence** — capture-only extension past the depth limit so the eval
  isn't taken mid-tactic.
- **Null move** — passing; if even passing keeps you above beta, the
  position is so good the branch can be cut.
- **LMR/LMP** — late-move reduction/pruning: moves ordered late are
  statistically bad, so search them cheaper (or skip them near leaves).
- **MVV-LVA** — order captures by Most Valuable Victim, Least Valuable
  Attacker (win material cheaply first).
- **Aspiration window** — start the search in a narrow score window around
  the last depth's result; widen and retry on failure.
- **SMP** — symmetric multiprocessing; here, Lazy SMP: threads search the
  same position at staggered depths sharing one TT.
- **Zobrist hash** — incremental-friendly position fingerprint (§1).
- **Perft** — leaf-node counter proving movegen correctness (§2).
- **Elo gate** — self-play match deciding whether a change is kept (§6).
