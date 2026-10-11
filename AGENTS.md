# AGENTS.md — dinochess (DinoChess)

Rust UCI chess engine. Std-only (zero deps by design — don't add crates).
`src/`: `types` → `board` → `movegen` → `eval` → `search` → `uci` → `main`
(`bitboard` serves `board`/`movegen`; `dino` holds mascot facts for `uci`).

## Toolchain (Windows PowerShell 5.1)

- `cargo` is NOT on PATH. Prefix every shell invocation:
  `$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path; cargo ...`
- No `&&`, no `head`; chain with `;`, page with `Select-Object -First/-Last`.

## Verify

- `cargo test` — perft correctness (startpos d1–4, Kiwipete, endgame) + eval tests.
  Dev profile sets `opt-level = 1` so perft d4 stays fast; don't remove it.
- `cargo build --release` — GUI binary: `target\release\dinochess.exe`.
- CLI shortcuts bypass the UCI loop (preferred for testing):
  `dinochess.exe perft <depth> [fen]`,
  `dinochess.exe search <depth> [fen]`,
  `dinochess.exe bench [depth]`.

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
  stored rank8-first and read owner-perspective (tapered `pawn/knight/bishop/
  rook/queen/king_pst` blend `(mg*phase + eg*(24-phase))/24`), signed once by
  color. Negating black's table value AND the total double-counts — this bug
  happened before. Symmetric positions differ only by the `TEMPO` side-to-move
  term (startpos evaluates to exactly `TEMPO`, not 0).
- EP captures require an enemy pawn on the jumped square (`ep_capture_ok` in
  `movegen.rs`); stale EP squares in FENs must NOT emit captures (phantom EPs
  once poisoned the board and lost 13 games on illegal king captures).
  `make/unmake_move` clear/restore captures by recorded owner (`cap.color`),
  never by assumed enemy.
- Mate scores: `MATE = 100_000`, `MATE_BOUND` threshold; TT store adds `ply`,
  probe subtracts it. Root stores its result to TT each iteration (that's what
  feeds PV collection); stalemate/no-move at root returns `bestmove 0000`.
- Repetition/fifty-move/insufficient-material return 0 inside search but must never
  affect `perft` counts.
- Classic chess only: no variants or spinoffs (Chess960, atomic, …) — rule of
  thumb. Movegen assumes standard castling/EP; keep it that way.
- NNUE (when P9 starts) stays std-only: bullet training + data live outside
  the repo and are never committed; only quantized weights ride along via
  `include_bytes!` — no crates for it. Accumulator updates belong in the
  make/unmake paths with an exact full-recompute cross-check test (desync is
  the same silent-corruption class as phantom-EP).

## Vendored dirs

`cutechess-1.5.1-win64/` (match GUI) and `stockfish/` (UCI reference + wiki) are
untracked helper tools, not part of the cargo build. Don't move engine code there.
