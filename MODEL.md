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
| `movegen.rs` | `generate_pseudo/legal/captures`, `perft`, `ep_capture_ok`, `see` |
| `eval.rs` | `evaluate()` (White view), tapered PSTs, structure/mobility/shield/ring-attack, `TEMPO` |
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
| QSearch delta margin | 150 | Captures skipped if SEE+static+150 ≤ alpha (promos never) |
| History | ±depth², clamp ±1,000,000 | Plain + counter-history + follow-up bonus on cutoff, gravity malus on quiet fail-low |
| King-ring attack | P/N/B/R/Q = 1/2/2/3/5 per attacker, ×10cp ×phase/24 | Enemy attacks on adjacent squares; each piece once |
| Ordering scores | hash 10M, promo 900k+victim·10, SEE≥0 captures MVV-LVA 1M+victim·10−attacker, killers 800k/790k, counter 785k, losing captures 700k+SEE, quiets history+ch+fh | |
| Time | `movetime−(mt/10∈[30,200])`; clocks `own/min(movestogo,50)+inc/2` minus reserve `(own/10∈[100,1000])`; soft deadline 65% | Bare `go` = depth 6 |

## TT entry (63 packed bits + atomics)

`Slot { key, data, check: AtomicU64 }`, `check = key ^ data ^ SALT`
(`0x9E3779B97F4A7C15`; torn reads fail closed). `data`: movecode 16b
(`0`=none else `1+(from|to≪6|promo≪12)`, promo=`idx+1`) | score+2²⁰ 21b |
depth 8b | flag 2b | generation 16b (bits 47–62; 0 = cleared). Fresh
generation supersedes stale same-key entries, depth-preferred within one
generation; `new_search()` bumps per `search_root`, `clear()` resets to 0.
Mate scores ply-adjusted (store +, probe −); root stores every iteration
(feeds PV).

## Reference numbers

| Check | Expected |
|---|---|
| `cargo test` | 49 passed |
| `perft 4` startpos | 197281 (~25ms release) |
| Kiwipete d3 / pos3 / endgame | 97862 / exact / exact (see tests) |
| `bench 8` | 823,390 nodes, ~575k nps (±8% run noise) |
| Gates | tapered kept; gravity 32–19–9 (+76 ±92); 200-game attempt exposed EP bug; TT aging neutral (52–48); SEE kept (23–21–16 @40/10, depth-12 Ba6>dxe6 proof); ring-attack kept (96–104 @40/10 neutral, Dragon sensed + Kirin f6 fixed); counter/follow-history kept (97.5–102.5 @40/10 neutral, −4.6% bench nodes, zero anomalies) |

## Test inventory (48)

perft: startpos d1–4, Kiwipete (+d4), pos3 deep, endgame, captures-vs-filter
(+children) · eval: balance/symmetry (mirrored side), queen, passer
(blocked/beats), isolated, rook-open, king-centralize, shield (unit +
intact/cracked), passer-king-distance, in-square, mobility, attack-sets
(mobility equivalence), ring-pressure (Dragon 10-vs-0), king-attack-moves-eval
· TT: pack round-trip, probe/store/collision, generation-prefers-fresh,
threaded smoke, SMP legality · SEE: hanging/defended/pawn-takes-queen/
even-exchange/xray-pair/EP/promotions, losing-capture-sorts-below-killer
· board: make/unmake round-trip (+promotion), repetition counts, stale-EP
phantom.
