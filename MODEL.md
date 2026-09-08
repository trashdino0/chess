# Ferrum Model (detailed)

This file explains how the engine evaluates positions and searches. It complements `README.md` (user guide) and `AGENTS.md` (architecture invariants).

---

## 1. Board representation (`src/board.rs`)

- `pieces: [[u64; 6]; 2]` — per-color/per-piece bitboards, the sole
  placement store; `occ: [u64; 2]` + `occ_all: u64` rebuilt on every make.
  `piece_at(sq)` is the mailbox replacement (occupancy fast path + one
  type scan). Square index `0 = a1`, `63 = h8`.
- `side: Color` (`White`/`Black`), `castling: u8` (bits `WK=1`, `WQ=2`, `BK=4`, `BQ=8`), `ep: Option<u8>`.
- `hash: u64` (Zobrist), `hash_history: Vec<u64>` (includes initial + after each `make`, including null moves).
- Zobrist uses deterministic `splitmix64` (seed `0x243F6A8885A308D3`) with per-piece/per-square/per-castling/per-EP-file keys.
- Undo (`Undo`) stores previous `move`, `captured`, `captured_sq`, `castling`, `ep`, `half`, `hash`.

---

## 2. Move structure (`src/types.rs`)

```rust
pub struct Move {
    pub from: u8,
    pub to: u8,
    pub promotion: Option<PieceType>,
}
```

No special flag bits. Castling is inferred in `make_move`: if a `King` moves 2 files horizontally, the corresponding `Rook` is relocated (e.g., `e1→g1` moves `h1` rook to `f1`). En passant is inferred: if a `Pawn` captures to `ep` but the target square is empty, the captured pawn is on the square behind (`from - 8` for White, `from + 8` for Black). Double push sets `ep` to the square between the start/end.

---

## 3. Evaluation (`src/eval.rs`)

### 3.1 Phase (middlegame → endgame)

`phase = weight(Pawn=0, Knight/Bishop=1, Rook=2, Queen=4, King=0)`, clamped to `[0, 24]`. At `phase = 24` the position is full-material (all 4 knights, 4 bishops, 4 rooks, 2 queens active); at `phase = 0` only kings remain.

The phase drives the `King` PST interpolation, the pawn-shield term (`shield_diff * phase / 24`, full middlegame → 0 endgame) and the passer king-distance term (`(24 - phase)` scaling, full endgame → 0 middlegame); other terms (`PAWN`, `KNIGHT`, etc.) use fixed middlegame tables.

### 3.2 Mobility / shield / passer distance

- Mobility (`MOB_KNIGHT/BISHOP/ROOK = 2`, `MOB_QUEEN = 1` cp per pseudo-legal destination, via `movegen::mobility_count`; pawns/kings excluded, owner-perspective signed). Symmetric; startpos contributes 0.
- Pawn shield (`king_shield`: +8 per own pawn one rank ahead of a home king on the 3 neighboring files, +4 two ranks ahead; kings tracked in `evaluate`'s first pass, no extra scans). Wired `(w - b) * phase / 24`.
- Passer king distance (inside `pawn_structure` for passers only): `(d_enemy - d_own) * 6 * (24 - phase) / 24` with Chebyshev distances to the promotion square; skipped when a king is missing. Quantifies the "square" rule.
- `development_bonus` is owner-signed (a developed Black piece scores for Black); `eval_is_color_symmetric` pins exact negation on a color-flipped mirror.

### 3.3 Tapered King (`KING_MG` / `KING_EG`)

```rust
fn king_pst(sq, color, phase) -> i32 {
    let mg = KING_MG[...];
    let eg = KING_EG[...];
    (mg * phase + eg * (24 - phase)) / 24
}
```

- `KING_MG`: central squares (`e5`, `d5`, etc.) are best; corners worst (`-50`).
- `KING_EG`: very central (`d4`, `e4`, `d5`, `e5`) best (`+40`); edges/corners negative (`-50`).
- At `phase = 24` the king behaves like the middlegame table; at `phase = 0` it pulls toward the center (endgame activity).

---

## 4. Search (`src/search.rs`)

### 4.1 Root (`search_root`)

- `search_root(board, tt, limits, threads, poll)` runs iterative deepening
  `depth = start..max_d` (`start = 1` on main, staggered deeper on helpers).
- `root_moves = generate_legal(board)`; ordered by `order_moves()`.
- Aspiration windows from depth 4: `(prev - delta, prev + delta)`, `delta = 25`. On fail-low (`sc <= alpha`): `alpha -= delta`, `delta *= 2`, retry. On fail-high (`sc >= beta`): `beta += delta`, `delta *= 2`, retry. After `prev.abs() >= MATE_BOUND` aspiration is disabled (mate scores + narrow windows cause useless re-searches).
- `root_search()` uses PVS: first move full window (`-beta, -alpha`); others null window (`-alpha - 1, -alpha`) then full window only if `sc > alpha` and `sc < beta`.
- Root stores best to `tt` (`self.tt.store(...)`) each iteration (this feeds PV collection).
- `bestmove 0000` returned when `root_moves.is_empty()` (stalemate/no moves at root) — `score` is `-MATE` if `in_check`, else `0`.
- Each completed iteration publishes `(depth, best, score)` to the shared
  slot (§4.3); the reported result is the deepest published one, falling
  back to the main thread's local result when no thread finished (e.g.
  piped-stdin abort during depth 1).

### 4.3 Lazy SMP (`threads >= 2`, `std::thread::scope`)

- Helpers get an owned `Board` clone, a TT handle clone (`Arc`), a `Limits`
  clone and their `tid`; `start_depth = min(1+tid, max_d)`; own
  killers/history/counter/stack and own `Instant` deadline built from the
  shared `think_ms: Option<u64>` (never share an `Instant`); silent (main
  prints only); abort via a `|| stop.load()` closure through the unchanged
  `tick(poll)` signature.
- Shared state is `Arc<Shared { stop: AtomicBool, best: Mutex<SharedBest>,
  nodes: Vec<AtomicU64> }>`: the main thread's channel-draining `poll`
  sets `stop` on stop/quit/disconnect; helpers publish node counts every
  1024 nodes in `tick` (+ flush at exit), so `info` nps shows combined
  effort. Measured: startpos `movetime 5000` Threads=1 depth 13 @893k nps
  vs Threads=4 depth 15 @3.19M nps (+2 ply in the same time).

### 4.2 Negamax (`negamax`)

- `ply` starts at 1 (`ply=0` is root, not recursed into here).
- Draw detection (before search): `half >= 100` (fifty-move), `is_insufficient_material()` (only kings + <=1 minor per side), `hash_count() >= 2` (repetition at `ply > 0`). Draws return `0`; they never affect `perft`.
- Check extension: `depth += 1` when `in_check`. Capped (never exceeds `MAX_PLY`).
- Futility pruning (`!in_check`, `depth <= 2`, `static_eval.abs() < MATE_BOUND`, `static_eval + margin <= alpha`): skips capture-only quiescence when a quiet move can't recover. Margin = `120` at depth 1, `250` at depth 2.
- Null-move pruning (`null_ok`, `depth >= 3`, `!in_check`, `ply > 0`, `static_eval >= beta`, `has_non_pawn_material()`): `r = 2 + (depth > 6)`, `sc = -negamax(board, depth - 1 - r, -beta, -beta + 1, ply + 1, false, poll)`. If `sc >= beta` and `sc.abs() < MATE_BOUND`, return `beta` immediately.
- Late-move pruning (`!is_cap`, `!is_killer`, `!in_check`, `depth <= 2`, `i >= 6 + depth * 6`): skip very late quiets near leaves.
- Move generation order: hash move (10M) > promotion (900k + victim*10) > captures (MVV-LVA: `1_000_000 + victim*10 - attacker`) > EP (`1_000_000 + pawn - pawn`) > killers (`800_000` / `790_000`) > countermove (`785_000`, quiet reply to the previous move via `counter[prev.from][prev.to]`, tracked on a per-ply move `stack`) > history score.
- Killer moves: `killers[ply]` array (`[Option<Move>; 2]`). Only quiet moves (`!cap`, `!ep`, `!promo`) update killers/history/countermove.
- Internal iterative reduction: `depth -= 1` at `depth >= 6` with no hash move (all node types; seeds the TT for ordering).
- History aging: `*h = (*h + depth*depth).min(1_000_000)`.
- `note_cutoff()` updates killers/history for quiet moves; `ply < MAX_PLY` guard prevents overflow.
- `quiescence()`: `in_check` → all legal moves; else captures + promotions only (`generate_captures()`). Delta pruning skips captures when `victim + stand + 150 <= alpha` (promotions never skipped). `alpha` updated with `stand`; return `alpha` (not `stand`) when no captures improve it.
- `quiescence` capped at `MAX_PLY + 24`; `ply >= MAX_PLY + 24` returns `side_eval()`.
- Mate distance pruning: `alpha = max(alpha, -MATE + ply)`, `beta = min(beta, MATE - ply)`; if `alpha >= beta`, return `alpha`.

---

## 5. Move generation (`src/movegen.rs`)

- `generate_pseudo()` walks the bitboards type-major (P/K via lookup
  tables, sliders via ray attacks), pushing to `Vec<Move>` (capacity `64`).
- `generate_pseudo_captures()` emits captures/EP/all promotions only (no quiets, no castling); `generate_captures()` legality-checks just that set. (Also fixed an old-filter bug: quiet non-pawn moves landing on the EP square were misclassified as captures.)
- `generate_legal()` filters pseudo-moves by `make_move` + `!in_check(side)` + `unmake_move`.
- `mobility_count()` (+ `count_stepper`/`count_slider`) mirrors the generators allocation-free for eval.
- `perft()` returns `1` at depth `0`, `moves.len()` at depth `1`, recursive `sum` otherwise.
- Perft correctness verified: `startpos` d1=20, d2=400, d3=8_902, d4=197_281; `kiwipete` d1=48, d2=2_039, d3=97_862; `endgame` d1=14, d2=191, d3=2_812.

---

## 6. UCI protocol (`src/uci.rs`)

- `Engine`: `board`, `tt` (default 16 MB), `hash_mb`, `threads`
  (default 1), `quit_after_search` (`Arc<AtomicBool>`). Identity: Ferrum 2.0.
- `spawn_stdin_reader()` creates a dedicated `std::thread` that locks `stdin` and sends lines over `mpsc::Sender<String>`.
- `run()` main loop: receives from `rx.recv()` (blocking) or `pending.pop_front()` (deferred commands from previous `go`).
- `handle_command()` matches `uci`, `isready`, `setoption` (`hash` and
  `threads`: `option name Threads type spin default 1 min 1 max 256`), `ucinewgame`, `position` (fen + moves), `go` (calls `run_search()`), `stop` (sets abort via poll callback), `quit`/`exit` (returns `true` to break loop), `d` (pretty print), `eval` (white-perspective `evaluate()`), `perft`, `bench`.
- `run_search()` builds `poll` closure that drains `rx.try_recv()`; on `stop` returns `true` (abort); on `quit`/`exit` sets `quit_after_search` and returns `true`; mid-search `isready` deferred; `position`/`ucinewgame` queued in `pending`. Search runs with `eng.threads` (§4.3). Search completes, deferred `isready` flushed as `readyok`, bestmove printed (`bestmove 0000` when `res.best` is `None`).
- `Limits`: `max_depth()` defaults to `64` when `depth` is `None`; `think_time_ms()` computes time per move from `wtime`/`btime`/`winc`/`binc` (`own / n.min(50)` with `movestogo`); clamped to `[10, 60_000*30]`. `movetime` overrides everything. Bare `go` (no time, no depth, not infinite) defaults to `depth = Some(6)` (quick terminal test).
- `parse_go()` skips unknown tokens; `searchmoves` skips tokens until next known keyword; `ponder`/`nodes`/`mate` handled with appropriate skips.
- `apply_position()` parses `fen` (joins first 4 tokens before `moves`) or `startpos`, applies `moves` (validates against `generate_legal()`, stops at first illegal), sets `eng.board`.
- `void_fen_check()` is a placeholder (keeps helper grouping).

---

## 7. Key design choices / limitations

- Zero external crates: no SIMD, no neural network. Bitboards throughout;
  Lazy SMP via the `Threads` UCI option (default 1; `bench`/`search` CLI run
  single-threaded for determinism).
- `generate_legal()` creates full pseudo-legal vector (`Vec::with_capacity(64)`) then filters by `in_check()`. Simple but not fastest.
- Transposition table: lock-free shared `Arc<Vec<Slot>>` (`Slot` =
  3×`AtomicU64`: key/data/xor-checksum, torn reads fail closed as misses),
  `mask = (n - 1)`, depth-preferred replacement (empty slot / key collision /
  `new.depth >= old.depth`). No generation aging yet.
- Repetition: `hash_counts` identity-hashed map synced in make/unmake (incl. nulls); `hash_count()` is O(1). `hash_history` retained for debugging.
- No opening book, no Syzygy tablebase integration, no endgame-specific search extensions beyond the tapered king PST.
- Search abort: `poll()` checks `nodes & 1023 == 0` and time/deadline; if `poll()` returns `true` or time exceeded, `Err(Abort)` bubbles up through `negamax`, `root_search`, `iterate`, and breaks `loop`, keeping previous iteration's `best`/`score`.
- `bestmove 0000` only at root stalemate (no legal moves + no check = stalemate; no legal moves + check = mate, `-MATE`).
