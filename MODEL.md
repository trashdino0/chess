# Ferrum Model (cheat-sheet)

One-line technical reference. Prose and rationale live in `docs.md`;
contributor rules in `AGENTS.md`; roadmap in `plan.md` (local-only).

## Module DAG

`types` → `board` → `movegen` → `eval` → `search` → `uci` → `main`.
`bitboard` (attack tables, rays, `pop_lsb`) serves `board`/`movegen`.

| File | Owns |
|---|---|
| `types.rs` | `Color`, `PieceType`, `Move{from,to,promotion}`, square helpers (`0=a1`) |
| `board.rs` | `pieces[[u64;6];2]` + `occ[2]` + `occ_all`, FEN, Zobrist, `make/unmake_move/null`, `Undo`, draw state |
| `movegen.rs` | `generate_pseudo/legal/captures`, `mobility_count`, `perft`, `ep_capture_ok` |
| `eval.rs` | `evaluate()` (White view), tapered PSTs, structure/mobility/shield, `TEMPO` |
| `search.rs` | `search_root`, `negamax`, `quiescence`, lock-free `TT`, Lazy SMP, `Limits` |
| `uci.rs` | `Engine`, command loop, `parse_go`, `apply_position`, `BENCH_FENS` |

## Key constants

| Symbol | Value | Meaning |
|---|---|---|
| `MATE` / `MATE_BOUND` / `INF` | 100000 / 99000 / 1000000 | Mate score / mate threshold / ceiling |
| `MAX_PLY` | 64 (+24 quiescence headroom) | Search depth cap |
| `TEMPO` | 12 | Side-to-move bonus (startpos evals to exactly this) |
| Phase weights | N/B 1, R 2, Q 4 (max 24) | MG→EG blend `(mg·p + eg·(24−p))/24` |
| Aspiration | depth ≥ 4, Δ=25, doubling | Disabled once a mate score appears |
| Null move | depth ≥ 3, R = 2 + (depth>6) | Needs static ≥ beta + non-pawn material, never mates |
| Futility margins | 120 (d1) / 250 (d2) | Frontier drop to quiescence |
| LMP | depth ≤ 2, skip quiet i ≥ 6+depth·6 | Non-killers near leaves |
| LMR | depth ≥ 3, late quiets −1 (−2 if depth ≥ 6, i ≥ 8) | Re-search on alpha exceed |
| QSearch delta margin | 150 | Captures skipped if victim+static+150 ≤ alpha |
| History | ±depth², clamp ±1,000,000 | Bonus on cutoff, gravity malus on quiet fail-low |
| Ordering scores | hash 10M, promo 900k+victim·10, MVV-LVA 1M+victim·10−attacker, killers 800k/790k, counter 785k | |
| Time | `movetime−30` (floor 10); clocks `own/min(movestogo,50)` (default ÷20) `+ inc/2` | Bare `go` = depth 6 |

## TT entry (47 packed bits + atomics)

`Slot { key, data, check: AtomicU64 }`, `check = key ^ data ^ SALT`
(`0x9E3779B97F4A7C15`; torn reads fail closed). `data`: movecode 16b
(`0`=none else `1+(from\|to≪6\|promo≪12)`, promo=`idx+1`) \| score+2²⁰ 21b \|
depth 8b \| flag 2b. Depth-preferred replacement; mate scores ply-adjusted
(store +, probe −); root stores every iteration (feeds PV).

## Reference numbers

| Check | Expected |
|---|---|
| `cargo test` | 34 passed |
| `perft 4` startpos | 197281 (~25ms release) |
| Kiwipete d3 / pos3 / endgame | 97862 / exact / exact (see tests) |
| `bench 8` | 2,909,536 nodes, ~700–835k nps (±8% run noise) |
| Gates | tapered kept; gravity 32–19–9 (+76 ±92); 200-game attempt exposed EP bug |

## Test inventory (34)

perft: startpos d1–4, Kiwipete (+d4), pos3 deep, endgame, captures-vs-filter
(+children) · eval: balance/symmetry (mirrored side), queen, passer
(blocked/beats), isolated, rook-open, king-centralize, shield (unit +
intact/cracked), passer-king-distance, in-square, mobility · TT: pack
round-trip, probe/store/collision, threaded smoke, SMP legality · board:
make/unmake round-trip (+promotion), repetition counts, stale-EP phantom.
