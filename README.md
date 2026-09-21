# Ferrum (chess-engine) — UCI engine

A zero-dependency Rust chess engine (`std` only) with UCI protocol support, iterative-deepening negamax search, evaluation using tapered piece-square tables, and a small set of structural/search heuristics. Designed for clarity and reproducibility rather than maximum Elo.

## Quick start

Requires: `cargo` (Windows PowerShell 5.1; prefix with path if not in `PATH`).

```powershell
# Build
$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path
cargo build --release

# Tests (perft, eval)
cargo test

# CLI shortcuts (bypass UCI loop):
.\target\release\chess-engine.exe bench [depth]
.\target\release\chess-engine.exe search [depth] [fen]
.\target\release\chess-engine.exe perft [depth] [fen]
```

## UCI session (interactive, not piped stdin)

Piped stdin (`echo ... | exe`) always yields depth-1-only searches because `EOF`/`disconnect` sets the abort flag (see `uci.rs`). For full searches, drive interactively with redirected stdin/stdout:

```powershell
$proc = Start-Process .\target\release\chess-engine.exe -PassThru -NoNewWindow -RedirectStandardInput tmp.in -RedirectStandardOutput tmp.out
```

Or use a GUI (Cutechess / Arena) that manages the process properly.

## Architecture (invariants)

- `Move` = `{from: u8, to: u8, promotion: Option<PieceType>}`. Castling / EP / double-push inferred inside `make_move` — never add flags to `Move`.
- Board: per-color/per-piece bitboards (`pieces[[u64;6];2]` + `occ[2]` + `occ_all`), FEN parse/generate, Zobrist hashing (`SplitMix64`), undo stack (`Undo`), 50-move / repetition (O(1) `hash_counts` map synced in make/unmake) / insufficient-material draw detection.
- Movegen: pseudo-legal (`generate_pseudo`) then legal (`generate_legal` via make/unmake + `in_check`). Quiescence uses direct capture-only generation (`generate_pseudo_captures`). Perft verified against standard positions.
- Eval (`src/eval.rs`): material + tapered piece-square tables for every piece (middlegame/endgame interpolation driven by game phase; king shelters early, activates late), pawn structure (passed/isolated/doubled, passers scored by king distance to promotion), rook open/semi-open + 7th-rank bonus, N/B/R/Q mobility, pawn-shield king safety (faded out in the endgame), bishop-pair bonus, side-to-move tempo. All terms symmetric (color-mirrored positions negate exactly); startpos evaluates to `TEMPO` (12).
- Search (`src/search.rs`): iterative deepening, aspiration windows (from depth 4, with widening on fail-high/low), lock-free shared transposition table (atomics + xor checksum, depth-preferred replacement), Lazy SMP via the `Threads` option (helpers on staggered depths, deepest result wins), null-move pruning (depth >=3, not in check, non-pawn material), internal iterative reduction (depth >=6, no hash move), check extension (capped at depth +1), quiescence with delta pruning (promotions/search-captures only; captures skipped if victim + static_eval + 150 <= alpha), late-move pruning (skip very late quiet non-killer moves near leaves), LMR (reduce late quiets 1–2 ply with re-search on alpha exceed), killer + countermove + history move ordering (history bonus for cutoff quiets, gravity malus for quiet fail-lows), MVV-LVA capture ordering.
- UCI (`uci.rs`): stdin-reader thread forwards lines over `mpsc`; `go` runs on main thread with periodic `poll()` checking the channel for `stop`/`quit`. `isready` deferred mid-search flushed after `go`. Options: `Hash` (MB), `Threads` (1–256, default 1).

## Benchmark / test helpers

- `stockfish/stockfish/` — vendored reference UCI engine (not part of cargo build).
- `cutechess-1.5.1-win64/` — vendored match harness (`cutechess-cli.exe`). Example:
  ```
  cutechess-cli.exe -engine cmd=... -each st=0.2 -games 48 -openings file=openings.epd format=epd order=random -draw movenumber=40 movecount=8 score=10 -resign movecount=4 score=800 -maxmoves 150 -concurrency 2 -pgnout out.pgn
  ```
- Note: `benchmarks/openings.epd` is clean (no comment lines) to avoid cutechess EPD parse warnings. One line is a synthetic French position with a stale EP square (no capturable pawn) — kept deliberately as regression coverage for phantom-EP generation (see `stale_ep_square_emits_no_phantom_capture`).

## Key files

- `Cargo.toml`: `edition = 2024`, std-only, `profile.release` `opt-level=3`, `profile.dev` `opt-level=1` (keeps perft d4 fast in tests).
- `AGENTS.md`: detailed architecture invariants, verification commands, and design notes.
- `src/types.rs`: colors, piece types, square indexing (`0 = a1`), castling rights bits (`WK`, `WQ`, `BK`, `BQ`).
- `src/board.rs`: bitboard board, FEN, Zobrist (`OnceLock<Tables>`), `make_move`/`unmake_move`/`make_null`, `is_square_attacked`, `in_check`, `is_draw`, `pretty()`.
- `src/movegen.rs`: pseudo-legal + legal generation, castling, promotion, `perft()`.
- `src/eval.rs`: tapered PSTs (`PAWN`/`KNIGHT`/`BISHOP`/`ROOK`/`QUEEN`/`KING_MG` + `_EG` mates, blended by phase) with `TEMPO` side-to-move term.
- `src/search.rs`: `search_root()`, `negamax()`, `quiescence()`, `TT`, `order_moves()`.
- `src/uci.rs`: `Engine`, command loop (`uci`, `position`, `go`, `stop`, `quit`, `perft`, `bench`, `d`, `eval`), `Limits`, `Limits.think_time_ms`.
- `src/main.rs`: CLI dispatcher (`perft`, `bench`, `search`) and UCI entry (`uci::run()`).

## Verification commands

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path
# Unit tests (perft + eval)
cargo test
# Release binary
cargo build --release
# Perft
.\target\release\chess-engine.exe perft 4
# Search
.\target\release\chess-engine.exe search 5
# Direct FEN search
.\target\release\chess-engine.exe search 6 "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1"
# Bench (release binary)
.\target\release\chess-engine.exe bench 6
```

## Known limitations / design choices

- Zero external crates by design; no SIMD, no neural network. `bench`/`search` CLI shortcuts run single-threaded for determinism.
- `generate_legal()` creates a full pseudo-legal vector (`Vec<Move>`), then filters by `in_check()` after `make_move`. This is simple but not the fastest possible.
- Transposition table is a lock-free shared power-of-two array with depth-preferred replacement; no generation aging yet.
- No opening book, no endgame tablebases, no learning / neural network.
- Search depth is bounded by `MAX_PLY` (64) and a time/deadline mechanism; `infinite` mode relies on `poll()`.
- EPD/opening files for cutechess must contain only FEN lines (no `#` comments); `benchmarks/openings.epd` is clean.
