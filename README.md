# DinoChess (dinochess) — UCI engine

A zero-dependency Rust chess engine (`std` only): classical alpha-beta
search with iterative deepening, aspiration windows, shared transposition
table, Lazy SMP, and a hand-written tapered evaluation. Built for clarity
and reproducibility rather than maximum Elo. Rawr.

## Quick start

Requires `cargo` (Windows PowerShell 5.1; prefix if not on `PATH`):

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path
cargo build --release   # GUI binary: target\release\dinochess.exe
cargo test              # 53 tests: perft + eval + TT + make/unmake + dino

# CLI shortcuts (deterministic, single-threaded — preferred for testing):
.\target\release\dinochess.exe perft 4
.\target\release\dinochess.exe search 8 [fen]
.\target\release\dinochess.exe bench 8
```

## Docs

- `docs.md` — how the engine works, end to end (start here to learn it).
- `MODEL.md` — maintainer's cheat-sheet: constants, layouts, reference numbers.
- `AGENTS.md` — contributor rules, verification commands, invariants.
- `plan.md` — improvement roadmap (local working file, not committed).

## UCI

Standard protocol (`uci`, `position`, `go`, `stop`, `quit`; options `Hash`,
`Threads`). Debug extensions: `d` (board), `eval`, `perft`, `bench`, `dino`
(serves a dinosaur fact). Drive sessions interactively or via GUI — piped stdin yields
depth-1-only searches (EOF aborts; correct behavior, see `docs.md` §5).

## Layout

`src/`: `types` → `board` → `movegen` → `eval` → `search` → `uci` → `main`.
`cutechess-1.5.1-win64/` and `stockfish/` are untracked helper tools, not
part of the build. No opening book, no tablebases, no external crates.
