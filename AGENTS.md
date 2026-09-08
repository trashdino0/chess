# AGENTS.md — chess-engine (Ferrum 1.0)

Rust UCI chess engine. Std-only (zero deps by design — don't add crates).
`src/`: `types` → `board` → `movegen` → `eval` → `search` → `uci` → `main`.

## Toolchain (Windows PowerShell 5.1)

- `cargo` is NOT on PATH. Prefix every shell invocation:
  `$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path; cargo ...`
- No `&&`, no `head`; chain with `;`, page with `Select-Object -First/-Last`.

## Verify

- `cargo test` — perft correctness (startpos d1–4, Kiwipete, endgame) + eval tests.
  Dev profile sets `opt-level = 1` so perft d4 stays fast; don't remove it.
- `cargo build --release` — GUI binary: `target\release\chess-engine.exe`.
- CLI shortcuts bypass the UCI loop (preferred for testing):
  `chess-engine.exe perft <depth> [fen]`,
  `chess-engine.exe search <depth> [fen]`,
  `chess-engine.exe bench [depth]`.

## UCI testing gotcha

Piped stdin (`echo ... | exe`) always yields depth-1-only searches: EOF/disconnect
sets the abort flag, and a piped `quit` aborts mid-search. This is correct engine
behavior, not a bug. For full sessions, drive the process interactively
(`System.Diagnostics.Process` with redirected stdin/stdout + `OutputDataReceived`;
note async event delivery can reorder adjacent sub-millisecond lines — harness
artifact, ignore it). `stop`/`quit` during `go` are handled via the stdin-reader
thread + poll callback in `uci.rs`.

## Architecture invariants

- `Move` is `{from, to, promotion}` only. Castling / EP / double-push are inferred
  from board state inside `make_move` — never add flags to `Move`.
- `evaluate()` is White-perspective; `side_eval()` converts for negamax. PSTs are
  stored rank8-first and read owner-perspective (`pst_own`), signed once by color.
  Negating black's table value AND the total double-counts — this bug happened before.
- Mate scores: `MATE = 100_000`, `MATE_BOUND` threshold; TT store adds `ply`,
  probe subtracts it. Root stores its result to TT each iteration (that's what
  feeds PV collection); stalemate/no-move at root returns `bestmove 0000`.
- Repetition/fifty-move/insufficient-material return 0 inside search but must never
  affect `perft` counts.

## Vendored dirs

`cutechess-1.5.1-win64/` (match GUI) and `stockfish/` (UCI reference + wiki) are
untracked helper tools, not part of the cargo build. Don't move engine code there.
