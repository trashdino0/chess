# Ferrum (chess-engine) — UCI engine

A zero-dependency Rust chess engine (`std` only): classical alpha-beta
search with iterative deepening, aspiration windows, shared transposition
table, Lazy SMP, and a hand-written tapered evaluation. Built for clarity
and reproducibility rather than maximum Elo.

## Quick start

Requires `cargo` (Windows PowerShell 5.1; prefix if not on `PATH`):

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path
cargo build --release   # GUI binary: target\release\chess-engine.exe
cargo test              # 49 tests: perft + eval + TT + make/unmake

# CLI shortcuts (deterministic, single-threaded — preferred for testing):
.\target\release\chess-engine.exe perft 4
.\target\release\chess-engine.exe search 8 [fen]
.\target\release\chess-engine.exe bench 8
```

## Docs

- `docs.md` — how the engine works, end to end (start here to learn it).
- `MODEL.md` — maintainer's cheat-sheet: constants, layouts, reference numbers.
- `AGENTS.md` — contributor rules, verification commands, invariants.
- `plan.md` — improvement roadmap (local working file, not committed).

## UCI

Standard protocol (`uci`, `position`, `go`, `stop`, `quit`; options `Hash`,
`Threads`). Drive sessions interactively or via GUI — piped stdin yields
depth-1-only searches (EOF aborts; correct behavior, see `docs.md` §5).

## Layout

`src/`: `types` → `board` → `movegen` → `eval` → `search` → `uci` → `main`.
`cutechess-1.5.1-win64/` and `stockfish/` are untracked helper tools, not
part of the build. No opening book, no tablebases, no external crates.
