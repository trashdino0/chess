//! Negamax search with alpha-beta, iterative deepening, aspiration windows,
//! quiescence, transposition table, null-move pruning, LMR, check extensions,
//! killer + history move ordering and MVV-LVA capture ordering.

use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::bitboard::bit;
use crate::board::Board;
use crate::eval::evaluate;
use crate::movegen::{generate_captures, generate_legal, generate_pseudo};
use crate::types::*;

pub const MATE: i32 = 100_000;
pub const INF: i32 = 1_000_000;
/// Scores with |score| >= MATE_BOUND are treated as mate scores.
pub const MATE_BOUND: i32 = MATE - 1_000;
pub const MAX_PLY: usize = 64;

const TT_EXACT: u8 = 0;
const TT_LOWER: u8 = 1;
const TT_UPPER: u8 = 2;

// ---------------------------------------------------------------------------
// Limits / time control
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct Limits {
    pub depth: Option<u8>,
    pub movetime: Option<u64>,
    pub wtime: Option<u64>,
    pub btime: Option<u64>,
    pub winc: Option<u64>,
    pub binc: Option<u64>,
    pub movestogo: Option<u32>,
    pub infinite: bool,
}

impl Limits {
    /// Fixed search depth (default 64 when nothing else given).
    pub fn max_depth(&self) -> u8 {
        self.depth.unwrap_or(MAX_PLY as u8)
    }

    /// Milliseconds to think, or None for untimed (depth/infinite).
    pub fn think_time_ms(&self, side: Color) -> Option<u64> {
        if let Some(mt) = self.movetime {
            // Scale-aware safety margin: a flat 30ms dominates bullet
            // budgets, while a loaded machine needs more than 30ms of
            // headroom at long controls. 10% bounded to [30, 200].
            let margin = (mt / 10).clamp(30, 200);
            return Some(mt.saturating_sub(margin).max(10));
        }
        let (own, inc) = match side {
            Color::White => (self.wtime?, self.winc.unwrap_or(0)),
            Color::Black => (self.btime?, self.binc.unwrap_or(0)),
        };
        let base = match self.movestogo {
            Some(n) if n > 0 => own / n.min(50) as u64,
            _ => own / 20,
        };
        let mut t = base + inc / 2;
        // Scale-aware clock reserve: 10% of remaining time, bounded to
        // [100ms, 1s], so latency spikes under load can't forfeit on a
        // nearly-empty clock while costing <2% with plenty of time left.
        let reserve = (own / 10).clamp(100, 1000);
        if own > reserve {
            t = t.min(own - reserve);
        }
        Some(t.clamp(10, 60_000 * 30))
    }
}

// ---------------------------------------------------------------------------
// Transposition table (lock-free shared hash table, depth-preferred repl.)
// ---------------------------------------------------------------------------

/// Salt mixed into the xor checksum so zeroed slots read as misses: a fresh
/// slot has key = data = check = 0, but expects `0 ^ 0 ^ SALT != 0`.
const TT_SALT: u64 = 0x9E3779B97F4A7C15;

#[derive(Copy, Clone)]
struct Entry {
    best: Option<Move>,
    score: i32,
    depth: u8,
    flag: u8,
}

/// One lock-free slot. A torn cross-thread read fails the xor check and is
/// treated as a miss — never as garbage data (so no bogus best-moves reach
/// `make_move`). Single-thread behavior is bit-identical to the old table.
struct Slot {
    key: AtomicU64,
    data: AtomicU64,
    check: AtomicU64,
}

/// Pack best/score/depth/flag/generation into 63 bits of `data`: bits 0–15
/// movecode (`0` = None, else `1 + (from | to<<6 | promo<<12)` with
/// `promo = PieceType::idx()+1`, 0 = none), bits 16–36 `(score + 2^20)`
/// (21 bits; `|score| <= INF = 1M < 2^20`), bits 37–44 depth, 45–46 flag,
/// bits 47–62 generation (16 bits; 0 = cleared table, never a live search).
fn pack_tt(best: Option<Move>, score: i32, depth: u8, flag: u8, generation: u16) -> u64 {
    let movecode: u64 = match best {
        None => 0,
        Some(mv) => {
            let promo = match mv.promotion {
                None => 0,
                Some(pt) => pt.idx() as u64 + 1,
            };
            1 + (mv.from as u64 | (mv.to as u64) << 6 | promo << 12)
        }
    };
    debug_assert!(movecode < 65536);
    debug_assert!(score.unsigned_abs() <= INF as u32);
    let score_field = (score as i64 + (1 << 20)) as u64;
    debug_assert!(score_field < (1 << 21));
    movecode
        | score_field << 16
        | (depth as u64) << 37
        | ((flag as u64) & 3) << 45
        | (generation as u64) << 47
}

fn unpack_tt(data: u64) -> (Option<Move>, i32, u8, u8, u16) {
    let best = match (data & 0xFFFF) as u32 {
        0 => None,
        c => {
            let c = c - 1;
            let promotion = match (c >> 12) & 15 {
                2 => Some(PieceType::Knight),
                3 => Some(PieceType::Bishop),
                4 => Some(PieceType::Rook),
                5 => Some(PieceType::Queen),
                // Pawn (1) / King (6) promos are never stored; a torn word
                // that verifies anyway is astronomically unlikely, but map
                // it to a quiet move rather than inventing a promotion.
                _ => None,
            };
            Some(Move {
                from: (c & 63) as u8,
                to: ((c >> 6) & 63) as u8,
                promotion,
            })
        }
    };
    let score = ((data >> 16) & 0x1F_FFFF) as i32 - (1 << 20);
    let depth = ((data >> 37) & 0xFF) as u8;
    let flag = ((data >> 45) & 3) as u8;
    let generation = (data >> 47) as u16;
    (best, score, depth, flag, generation)
}

#[derive(Clone)]
pub struct TT {
    slots: Arc<Vec<Slot>>,
    mask: usize,
    /// Search generation, bumped by `new_search` at every `search_root`.
    /// Shared across clones (one table = one counter). 0 means "cleared";
    /// live searches run at 1..=u16::MAX with wraparound.
    generation: Arc<AtomicU16>,
}

impl TT {
    pub fn new(mb: usize) -> TT {
        let bytes = (mb.max(1).min(1024) * 1024 * 1024) as u64;
        // 24-byte slots; round slot count down to a power of two.
        let mut n = (bytes / 24).max(1024);
        n = n.next_power_of_two() >> 1;
        if n < 1024 {
            n = 1024;
        }
        let mut v = Vec::with_capacity(n as usize);
        for _ in 0..n {
            v.push(Slot {
                key: AtomicU64::new(0),
                data: AtomicU64::new(0),
                check: AtomicU64::new(0),
            });
        }
        TT {
            slots: Arc::new(v),
            mask: (n as usize) - 1,
            generation: Arc::new(AtomicU16::new(0)),
        }
    }

    /// Start a new search generation. Call once per `search_root` so a fresh
    /// search's entries outrank stale ones from previous searches/games on
    /// replacement (same class of staleness as bench cross-position
    /// pollution, but in real games). Wraps safely; 0 is reserved for
    /// cleared tables.
    pub fn new_search(&self) -> u16 {
        let mut g = self.generation.load(Ordering::Relaxed).wrapping_add(1);
        if g == 0 {
            g = 1;
        }
        self.generation.store(g, Ordering::Relaxed);
        g
    }

    pub fn clear(&self) {
        for s in self.slots.iter() {
            s.data.store(0, Ordering::Relaxed);
            s.check.store(0, Ordering::Relaxed);
            s.key.store(0, Ordering::Relaxed);
        }
        self.generation.store(0, Ordering::Relaxed);
    }

    pub fn hashfull(&self) -> u32 {
        let sample = self.slots.len().min(1000);
        if sample == 0 {
            return 0;
        }
        let mut used = 0;
        for s in self.slots.iter().take(sample) {
            let key = s.key.load(Ordering::Relaxed);
            let data = s.data.load(Ordering::Relaxed);
            let check = s.check.load(Ordering::Relaxed);
            if check == key ^ data ^ TT_SALT {
                used += 1;
            }
        }
        (used * 1000 / sample) as u32
    }

    fn probe(&self, key: u64) -> Option<Entry> {
        let s = &self.slots[(key as usize) & self.mask];
        let sk = s.key.load(Ordering::Acquire);
        if sk != key {
            return None;
        }
        let data = s.data.load(Ordering::Relaxed);
        let check = s.check.load(Ordering::Relaxed);
        if check != key ^ data ^ TT_SALT {
            return None;
        }
        let (best, score, depth, flag, _) = unpack_tt(data);
        Some(Entry {
            best,
            score,
            depth,
            flag,
        })
    }

    fn store(&self, key: u64, best: Option<Move>, score: i32, depth: u8, flag: u8) {
        let s = &self.slots[(key as usize) & self.mask];
        let cur = self.generation.load(Ordering::Relaxed);
        if s.key.load(Ordering::Relaxed) == key {
            // Same position: a fresh generation always supersedes a stale
            // entry (this search's own lines outrank last game's deep ones);
            // within one generation keep the deeper entry as before. An
            // unverified (torn) triple counts as an empty slot so it never
            // blocks a fresh store.
            let old_data = s.data.load(Ordering::Relaxed);
            let old_check = s.check.load(Ordering::Relaxed);
            if old_check == key ^ old_data ^ TT_SALT {
                let (_, _, old_depth, _, old_gen) = unpack_tt(old_data);
                if old_gen == cur && depth < old_depth {
                    return;
                }
            }
        }
        // Empty slots and key collisions always store. Order: data, check,
        // key last — a reader that observes the new key (Acquire) also sees
        // the new data/check, and any torn mix fails the xor check as a miss.
        let data = pack_tt(best, score, depth, flag, cur);
        s.data.store(data, Ordering::Relaxed);
        s.check.store(key ^ data ^ TT_SALT, Ordering::Relaxed);
        s.key.store(key, Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// Searcher
// ---------------------------------------------------------------------------

struct Abort;

pub struct SearchResult {
    pub best: Option<Move>,
    pub score: i32,
    pub nodes: u64,
}

/// Run iterative deepening on `board`, printing UCI `info` lines.
/// `poll()` is called periodically; returning true aborts the search
/// (used for the UCI `stop` command). Returns best move + score.
///
/// `threads >= 2` runs Lazy SMP: helper threads search owned board clones
/// sharing the TT, staggered to deeper start depths; the deepest completed
/// result across all threads is reported.
pub fn search_root(
    board: &mut Board,
    tt: &TT,
    limits: &Limits,
    threads: usize,
    poll: &mut dyn FnMut() -> bool,
) -> SearchResult {
    // Fresh generation per search: this search's stores outrank stale
    // entries from previous searches on replacement.
    tt.new_search();
    let n = threads.clamp(1, 256);
    // Shared by value, never shared by reference: each thread builds its own
    // `Instant` deadline from this (Instants are not shareable).
    let think_ms = limits.think_time_ms(board.side);
    let max_d = limits.max_depth();
    if n == 1 {
        let mut s = Searcher::new(tt.clone(), think_ms, max_d, 0, None, 1);
        return s.iterate(board, poll);
    }
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        best: Mutex::new(SharedBest {
            depth: 0,
            mv: None,
            score: 0,
        }),
        nodes: (0..n).map(|_| AtomicU64::new(0)).collect(),
    });
    // Owned clones for helpers (made up front: `thread::scope` can't hand
    // out `&mut` borrows of one Vec across iterations, but owned moves in).
    let helper_boards: Vec<Board> = (1..n).map(|_| board.clone()).collect();
    let main_res = std::thread::scope(|scope| {
        for (i, mut hb) in helper_boards.into_iter().enumerate() {
            let tid = i + 1;
            let tt = tt.clone();
            let lim = limits.clone();
            let shared = Arc::clone(&shared);
            scope.spawn(move || {
                helper_search(&mut hb, &tt, &lim, tid, &shared);
            });
        }
        // Main thread: keeps the channel-draining `poll`, and forwards any
        // abort to the helpers via shared stop.
        let mut main_poll = || {
            if poll() {
                shared.stop.store(true, Ordering::Relaxed);
                true
            } else {
                false
            }
        };
        let mut s = Searcher::new(tt.clone(), think_ms, max_d, 0, Some(Arc::clone(&shared)), 1);
        let res = s.iterate(board, &mut main_poll);
        shared.stop.store(true, Ordering::Relaxed);
        res
    });
    // Helpers joined. Deepest completed result wins; the local fallback
    // preserves the piped-stdin depth-1 behavior (abort before any iteration
    // completes leaves `shared.best` empty).
    let total_nodes = main_res.nodes
        + shared
            .nodes
            .iter()
            .skip(1)
            .map(|a| a.load(Ordering::Relaxed))
            .sum::<u64>();
    let locked = shared.best.lock().unwrap();
    let (best, score) = match locked.mv {
        Some(mv) => (Some(mv), locked.score),
        None => (main_res.best, main_res.score),
    };
    SearchResult {
        best,
        score,
        nodes: total_nodes,
    }
}

/// Deepest completed result published by any SMP thread.
struct SharedBest {
    depth: u8,
    mv: Option<Move>,
    score: i32,
}

struct Shared {
    stop: AtomicBool,
    best: Mutex<SharedBest>,
    /// Per-thread node counts (`nodes[0]` unused: the main thread reports
    /// its own count; helpers publish theirs every 1024 nodes + at exit).
    nodes: Vec<AtomicU64>,
}

/// One Lazy SMP helper: own board, own tables, shared TT, silent.
fn helper_search(board: &mut Board, tt: &TT, limits: &Limits, tid: usize, shared: &Arc<Shared>) {
    let think_ms = limits.think_time_ms(board.side);
    let max_d = limits.max_depth();
    let start_depth = (1 + tid as u8).min(max_d);
    let mut poll = || shared.stop.load(Ordering::Relaxed);
    let mut s = Searcher::new(
        tt.clone(),
        think_ms,
        max_d,
        tid,
        Some(Arc::clone(shared)),
        start_depth,
    );
    let res = s.iterate(board, &mut poll);
    // Flush the remainder (< 1024 nodes) that `tick` hasn't published yet.
    shared.nodes[tid].store(res.nodes, Ordering::Relaxed);
}

struct Searcher {
    tt: TT,
    killers: [[Option<Move>; 2]; MAX_PLY + 8],
    history: [[[i32; 64]; 64]; 2],
    /// Countermove table: for the previous move's from/to, the quiet move
    /// that last caused a cutoff in reply.
    counter: [[Option<Move>; 64]; 64],
    /// Move stack: `stack[ply]` is the move played from the node at `ply`
    /// (None for null moves / above the root); sized past MAX_PLY so
    /// quiescence (capped at MAX_PLY + 24) can index it too.
    stack: [Option<Move>; MAX_PLY + 32],
    nodes: u64,
    seldepth: u32,
    deadline: Option<Instant>,
    max_depth: u8,
    /// SMP identity (0 = main, prints; >= 1 = helper, silent).
    tid: usize,
    shared: Option<Arc<Shared>>,
    /// First iterative-deepening depth (helpers stagger deeper to diversify).
    start_depth: u8,
}

impl Searcher {
    fn new(
        tt: TT,
        think_ms: Option<u64>,
        max_depth: u8,
        tid: usize,
        shared: Option<Arc<Shared>>,
        start_depth: u8,
    ) -> Searcher {
        Searcher {
            tt,
            killers: [[None; 2]; MAX_PLY + 8],
            history: [[[0; 64]; 64]; 2],
            counter: [[None; 64]; 64],
            stack: [None; MAX_PLY + 32],
            nodes: 0,
            seldepth: 0,
            deadline: think_ms.map(|ms| Instant::now() + Duration::from_millis(ms)),
            max_depth,
            tid,
            shared,
            start_depth,
        }
    }

    fn silent(&self) -> bool {
        self.tid != 0
    }

    fn check_time(&self) -> bool {
        match self.deadline {
            Some(d) => Instant::now() >= d,
            None => false,
        }
    }

    #[inline]
    fn tick(&mut self, poll: &mut dyn FnMut() -> bool) -> Result<(), Abort> {
        if self.nodes & 1023 == 0 {
            // Publish helper progress for combined info-nps; the main
            // thread's own count stays authoritative for its total.
            if let Some(sh) = &self.shared {
                sh.nodes[self.tid].store(self.nodes, Ordering::Relaxed);
            }
            if self.check_time() || poll() {
                return Err(Abort);
            }
        }
        Ok(())
    }

    fn iterate(&mut self, board: &mut Board, poll: &mut dyn FnMut() -> bool) -> SearchResult {
        let start = Instant::now();
        let mut root_moves = generate_legal(board);
        if root_moves.is_empty() {
            // Already mated or stalemated.
            let score = if board.in_check(board.side) { -MATE } else { 0 };
            return SearchResult {
                best: None,
                score,
                nodes: 0,
            };
        }
        order_moves(
            board,
            &mut root_moves,
            None,
            None,
            0,
            &self.killers,
            &self.history,
            &self.counter,
        );

        let mut best = root_moves[0];
        let mut score = side_eval(board);
        let max_d = self.max_depth.min(MAX_PLY as u8);
        // Total budget for the soft deadline below (None = untimed depth
        // search, which always runs to completion).
        let budget = self.deadline.map(|dl| dl.saturating_duration_since(start));

        for depth in self.start_depth.max(1)..=max_d {
            // Soft deadline: don't start an iteration once 65% of the
            // budget is spent. A depth that can't finish contributes
            // nothing (its abort is discarded), while the poll loop it
            // burns still costs wall-clock near the flag. First iteration
            // always runs so there is always a searched best move.
            if depth > self.start_depth.max(1)
                && budget.is_some_and(|b| start.elapsed() * 100 > b * 65)
            {
                break;
            }
            let prev = score;
            // Aspiration windows from depth 4 on (skipped after a mate score:
            // mate scores + narrow windows only waste re-searches).
            let use_asp = depth >= 4 && prev.abs() < MATE_BOUND;
            let mut delta = 25;
            let (mut alpha, mut beta) = if use_asp {
                ((prev - delta).max(-INF), (prev + delta).min(INF))
            } else {
                (-INF, INF)
            };
            let r = loop {
                match self.root_search(board, &root_moves, depth as i32, alpha, beta, poll) {
                    Err(Abort) => break None,
                    Ok((sc, mv)) => {
                        if use_asp && sc <= alpha {
                            // Fail low: widen below, keep beta.
                            alpha = (alpha - delta).max(-INF);
                            delta *= 2;
                            continue;
                        } else if use_asp && sc >= beta {
                            // Fail high: widen above, keep alpha.
                            beta = (beta + delta).min(INF);
                            delta *= 2;
                            continue;
                        } else {
                            break Some((sc, mv));
                        }
                    }
                }
            };
            let Some((sc, mv)) = r else {
                break; // aborted: keep previous iteration's result
            };
            score = sc;
            best = mv;
            // Move best to front for the next iteration + PV printing.
            if let Some(i) = root_moves.iter().position(|&m| m == best) {
                root_moves[..=i].rotate_right(1);
            }
            // Store the root result so PV collection can walk it.
            // (Root ply = 0, so no mate-score adjustment is needed.)
            self.tt
                .store(board.hash, Some(best), score, depth, TT_EXACT);
            // Publish for SMP: deepest completed iteration wins.
            if let Some(sh) = &self.shared {
                let mut b = sh.best.lock().unwrap();
                if depth >= b.depth {
                    b.depth = depth;
                    b.mv = Some(best);
                    b.score = score;
                }
            }
            self.print_info(board, depth, score, &start);
            if score.abs() >= MATE_BOUND {
                break; // found forced mate; deeper search is pointless
            }
            if self.check_time() || poll() {
                break;
            }
        }
        SearchResult {
            best: Some(best),
            score,
            nodes: self.nodes,
        }
    }

    fn root_search(
        &mut self,
        board: &mut Board,
        moves: &[Move],
        depth: i32,
        mut alpha: i32,
        beta: i32,
        poll: &mut dyn FnMut() -> bool,
    ) -> Result<(i32, Move), Abort> {
        let mut best = moves[0];
        let mut best_score = -INF;
        for (i, &mv) in moves.iter().enumerate() {
            self.stack[0] = Some(mv);
            let undo = board.make_move(mv);
            // Unmake runs before `?` can propagate: aborts unwind through
            // here, and every `make` must meet its `unmake` so the board is
            // never left dirty (same pattern at every search call site).
            let sc = if i == 0 {
                let res = self.negamax(board, depth - 1, -beta, -alpha, 1, true, poll);
                board.unmake_move(undo);
                -res?
            } else {
                // Principal variation search: null window first.
                let res = (|| -> Result<i32, Abort> {
                    let mut sc =
                        -self.negamax(board, depth - 1, -alpha - 1, -alpha, 1, true, poll)?;
                    if sc > alpha && sc < beta {
                        sc = -self.negamax(board, depth - 1, -beta, -alpha, 1, true, poll)?;
                    }
                    Ok(sc)
                })();
                board.unmake_move(undo);
                res?
            };
            if sc > best_score {
                best_score = sc;
                best = mv;
            }
            let failed_low = sc <= alpha;
            if sc > alpha {
                alpha = sc;
            }
            if alpha >= beta {
                self.note_cutoff(board.side, mv, board, 0, depth);
                break;
            }
            // History gravity: a quiet move that failed low loses points, so
            // stale history can't outrank newly-good moves forever.
            // (Post-unmake occupancy is the pre-move state, matching
            // `note_cutoff`'s quiet check.)
            let quiet = board.occ_all & bit(mv.to) == 0
                && Some(mv.to) != board.ep
                && mv.promotion.is_none();
            if quiet && failed_low {
                self.history_malus(board.side, mv, depth);
            }
        }
        Ok((best_score, best))
    }

    #[allow(clippy::too_many_arguments)]
    fn negamax(
        &mut self,
        board: &mut Board,
        mut depth: i32,
        mut alpha: i32,
        mut beta: i32,
        ply: usize,
        null_ok: bool,
        poll: &mut dyn FnMut() -> bool,
    ) -> Result<i32, Abort> {
        self.nodes += 1;
        self.tick(poll)?;

        if ply >= MAX_PLY {
            return Ok(side_eval(board));
        }

        // Draws.
        if board.is_fifty_move() || board.is_insufficient_material() {
            return Ok(0);
        }
        if ply > 0 && board.hash_count() >= 2 {
            return Ok(0);
        }

        let in_check = board.in_check(board.side);

        // Mate distance pruning.
        alpha = alpha.max(-MATE + ply as i32);
        beta = beta.min(MATE - ply as i32);
        if alpha >= beta {
            return Ok(alpha);
        }

        if depth <= 0 {
            return self.quiescence(board, alpha, beta, ply, poll);
        }

        // Check extension (capped so checking lines can't explode the tree).
        if in_check && depth < MAX_PLY as i32 {
            depth += 1;
        }

        let orig_alpha = alpha;
        let key = board.hash;

        // TT probe.
        let mut hash_move = None;
        if let Some(e) = self.tt.probe(key) {
            if e.depth as i32 >= depth {
                let mut s = e.score;
                if s >= MATE_BOUND {
                    s -= ply as i32;
                } else if s <= -MATE_BOUND {
                    s += ply as i32;
                }
                match e.flag {
                    TT_EXACT => return Ok(s),
                    TT_LOWER => alpha = alpha.max(s),
                    _ => beta = beta.min(s),
                }
                if alpha >= beta {
                    return Ok(s);
                }
            }
            hash_move = e.best;
        }

        // Internal iterative reduction: with no hash move to order by at
        // high depth, shave one ply so the search isn't flying blind (the
        // reduced search still seeds the TT for a re-search). Conservative
        // and depth-gated; applies at all node types.
        if hash_move.is_none() && depth >= 6 {
            depth -= 1;
        }

        // Null-move pruning.
        let static_eval = side_eval(board);
        if null_ok
            && depth >= 3
            && !in_check
            && ply > 0
            && static_eval >= beta
            && has_non_pawn_material(board)
        {
            let r = 2 + (depth > 6) as i32;
            let undo = board.make_null();
            self.stack[ply] = None; // null move: no countermove info below
            let res = self.negamax(board, depth - 1 - r, -beta, -beta + 1, ply + 1, false, poll);
            board.unmake_null(undo);
            let sc = -res?;
            if sc >= beta && sc.abs() < MATE_BOUND {
                return Ok(beta);
            }
        }

        // Futility pruning: at the frontier a quiet move can't recover a
        // badly losing static eval, so search captures only (quiescence).
        if !in_check
            && depth <= 2
            && static_eval.abs() < MATE_BOUND
            && alpha.abs() < MATE_BOUND
            && beta.abs() < MATE_BOUND
        {
            let margin = if depth == 1 { 120 } else { 250 };
            if static_eval + margin <= alpha {
                return self.quiescence(board, alpha, beta, ply, poll);
            }
        }

        let mut moves = generate_legal(board);
        if moves.is_empty() {
            if in_check {
                return Ok(-MATE + ply as i32);
            }
            return Ok(0);
        }
        let prev = if ply > 0 { self.stack[ply - 1] } else { None };
        order_moves(
            board,
            &mut moves,
            hash_move,
            prev,
            ply,
            &self.killers,
            &self.history,
            &self.counter,
        );

        let mut best_move = None;
        let mut best_score = -INF;
        let killer1 = if ply < self.killers.len() {
            self.killers[ply][0]
        } else {
            None
        };
        let killer2 = if ply < self.killers.len() {
            self.killers[ply][1]
        } else {
            None
        };
        for (i, &mv) in moves.iter().enumerate() {
            let is_cap = board.occ_all & bit(mv.to) != 0
                || Some(mv.to) == board.ep
                || mv.promotion.is_some();
            let is_killer = Some(mv) == killer1 || Some(mv) == killer2;
            // Late-move pruning: skip very late quiets near the leaves.
            // Index 0 is always searched, so best_move is always set.
            if !is_cap && !is_killer && !in_check && depth <= 2 && best_score > -MATE_BOUND {
                if i >= 6 + depth as usize * 6 {
                    continue;
                }
            }
            self.stack[ply] = Some(mv);
            let undo = board.make_move(mv);
            // Late move reduction for quiet moves ordered late (never killers).
            // (Unmake-before-`?` as in root_search: aborts never dirty the board.)
            let sc = if i >= 3 && depth >= 3 && !is_cap && !is_killer && !in_check {
                let r = if depth >= 6 && i >= 8 { 2 } else { 1 };
                let res = (|| -> Result<i32, Abort> {
                    let mut sc = -self.negamax(
                        board,
                        depth - 1 - r,
                        -alpha - 1,
                        -alpha,
                        ply + 1,
                        true,
                        poll,
                    )?;
                    if sc > alpha {
                        sc = -self.negamax(board, depth - 1, -beta, -alpha, ply + 1, true, poll)?;
                    }
                    Ok(sc)
                })();
                board.unmake_move(undo);
                res?
            } else if i == 0 {
                let res = self.negamax(board, depth - 1, -beta, -alpha, ply + 1, true, poll);
                board.unmake_move(undo);
                -res?
            } else {
                let res = (|| -> Result<i32, Abort> {
                    let mut sc =
                        -self.negamax(board, depth - 1, -alpha - 1, -alpha, ply + 1, true, poll)?;
                    if sc > alpha && sc < beta {
                        sc = -self.negamax(board, depth - 1, -beta, -alpha, ply + 1, true, poll)?;
                    }
                    Ok(sc)
                })();
                board.unmake_move(undo);
                res?
            };

            if sc > best_score {
                best_score = sc;
                best_move = Some(mv);
            }
            let failed_low = sc <= alpha;
            if sc > alpha {
                alpha = sc;
            }
            if alpha >= beta {
                self.note_cutoff(board.side, mv, board, ply, depth);
                break;
            }
            // History gravity: a quiet move that failed low loses points, so
            // stale history can't outrank newly-good moves forever.
            // (`is_cap` was computed pre-move; killers are quiet by
            // construction and take the malus like any other quiet.)
            if !is_cap && failed_low && ply < MAX_PLY {
                self.history_malus(board.side, mv, depth);
            }
        }

        let flag = if best_score <= orig_alpha {
            TT_UPPER
        } else if best_score >= beta {
            TT_LOWER
        } else {
            TT_EXACT
        };
        let mut store_score = best_score;
        if store_score >= MATE_BOUND {
            store_score += ply as i32;
        } else if store_score <= -MATE_BOUND {
            store_score -= ply as i32;
        }
        self.tt
            .store(key, best_move, store_score, depth as u8, flag);
        Ok(best_score)
    }

    fn quiescence(
        &mut self,
        board: &mut Board,
        mut alpha: i32,
        beta: i32,
        ply: usize,
        poll: &mut dyn FnMut() -> bool,
    ) -> Result<i32, Abort> {
        self.nodes += 1;
        self.tick(poll)?;
        self.selduring(ply);

        if board.is_fifty_move() || board.is_insufficient_material() {
            return Ok(0);
        }
        if ply >= MAX_PLY + 24 {
            return Ok(side_eval(board));
        }

        let in_check = board.in_check(board.side);
        let stand = side_eval(board);
        if !in_check {
            if stand >= beta {
                return Ok(beta);
            }
            if stand > alpha {
                alpha = stand;
            }
        }

        // In check: search all evasions; otherwise only captures/promotions.
        let moves = if in_check {
            generate_legal(board)
        } else {
            generate_captures(board)
        };
        if moves.is_empty() {
            if in_check {
                return Ok(-MATE + ply as i32);
            }
            return Ok(alpha);
        }
        let mut ordered = moves;
        let prev = if ply > 0 && ply - 1 < self.stack.len() {
            self.stack[ply - 1]
        } else {
            None
        };
        order_moves(
            board,
            &mut ordered,
            None,
            prev,
            ply,
            &self.killers,
            &self.history,
            &self.counter,
        );

        for mv in ordered {
            if ply < self.stack.len() {
                self.stack[ply] = Some(mv);
            }
            let undo = board.make_move(mv);
            let res = self.quiescence(board, -beta, -alpha, ply + 1, poll);
            board.unmake_move(undo);
            let sc = -res?;
            if sc > alpha {
                alpha = sc;
            }
            if alpha >= beta {
                return Ok(beta);
            }
        }
        Ok(alpha)
    }

    fn selduring(&mut self, ply: usize) {
        if ply as u32 > self.seldength() {
            self.seldepth = ply as u32;
        }
    }

    fn seldength(&self) -> u32 {
        self.seldepth
    }

    fn note_cutoff(&mut self, side: Color, mv: Move, board: &Board, ply: usize, depth: i32) {
        // Only quiet moves update killers/history.
        let quiet =
            board.occ_all & bit(mv.to) == 0 && Some(mv.to) != board.ep && mv.promotion.is_none();
        // NB: board here is AFTER unmake (position before the move), so the
        // occupancy at `to` is the pre-move state. Captures have a victim there.
        if !quiet || ply >= MAX_PLY {
            return;
        }
        let k = &mut self.killers[ply];
        if k[0] != Some(mv) {
            k[1] = k[0];
            k[0] = Some(mv);
        }
        let h = &mut self.history[side.idx()][mv.from as usize][mv.to as usize];
        *h = (*h + depth * depth).min(1_000_000);
        // Countermove: reply `mv` to the move that led here.
        if ply >= 1 {
            if let Some(pm) = self.stack[ply - 1] {
                self.counter[pm.from as usize][pm.to as usize] = Some(mv);
            }
        }
    }

    /// History gravity: quiet fail-lows decay toward `-1_000_000`, mirroring
    /// the `+depth²` bonus (clamped at `+1_000_000`) in `note_cutoff`.
    fn history_malus(&mut self, side: Color, mv: Move, depth: i32) {
        let h = &mut self.history[side.idx()][mv.from as usize][mv.to as usize];
        *h = (*h - depth * depth).max(-1_000_000);
    }

    fn print_info(&mut self, board: &mut Board, depth: u8, score: i32, start: &Instant) {
        if self.silent() {
            return;
        }
        let ms = start.elapsed().as_millis().max(1) as u64;
        // Combined effort across threads so SMP scaling shows in nps.
        let helper_nodes: u64 = self
            .shared
            .as_ref()
            .map(|sh| {
                sh.nodes
                    .iter()
                    .skip(1)
                    .map(|a| a.load(Ordering::Relaxed))
                    .sum()
            })
            .unwrap_or(0);
        let shown_nodes = self.nodes + helper_nodes;
        let nps = shown_nodes * 1000 / ms;
        let score_str = if score.abs() >= MATE_BOUND {
            let plies = MATE - score.abs();
            let moves = (plies + 1) / 2;
            format!("mate {}", if score > 0 { moves } else { -moves })
        } else {
            format!("cp {score}")
        };
        let pv = collect_pv(board, &self.tt, depth as usize);
        let pv_str = pv.iter().map(|m| m.to_uci()).collect::<Vec<_>>().join(" ");
        println!(
            "info depth {} seldepth {} score {} nodes {} nps {} time {} hashfull {} pv {}",
            depth,
            self.seldepth,
            score_str,
            shown_nodes,
            nps,
            ms,
            self.tt.hashfull(),
            pv_str
        );
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
    }
}

fn collect_pv(board: &mut Board, tt: &TT, max_len: usize) -> Vec<Move> {
    let mut pv = Vec::new();
    let mut undos = Vec::new();
    for _ in 0..max_len {
        let entry = tt.probe(board.hash);
        let Some(e) = entry else { break };
        let Some(mv) = e.best else { break };
        if !generate_pseudo(board).contains(&mv) {
            break;
        }
        let side = board.side;
        let undo = board.make_move(mv);
        if board.in_check(side) {
            board.unmake_move(undo);
            break;
        }
        undos.push(undo);
        pv.push(mv);
        if board.hash_count() >= 3 {
            break;
        }
    }
    while let Some(u) = undos.pop() {
        board.unmake_move(u);
    }
    pv
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Static eval from the side-to-move's perspective.
#[inline]
fn side_eval(board: &Board) -> i32 {
    let e = evaluate(board);
    if board.side == Color::White {
        e
    } else {
        -e
    }
}

fn has_non_pawn_material(board: &Board) -> bool {
    let si = board.side.idx();
    board.pieces[si][PieceType::Knight.idx()]
        | board.pieces[si][PieceType::Bishop.idx()]
        | board.pieces[si][PieceType::Rook.idx()]
        | board.pieces[si][PieceType::Queen.idx()]
        != 0
}

fn order_moves(
    board: &Board,
    moves: &mut [Move],
    hash_move: Option<Move>,
    prev: Option<Move>,
    ply: usize,
    killers: &[[Option<Move>; 2]; MAX_PLY + 8],
    history: &[[[i32; 64]; 64]; 2],
    counter: &[[Option<Move>; 64]; 64],
) {
    let side = board.side;
    // Countermove for the move that led here (quiet replies only).
    let counter_move = prev.and_then(|pm| counter[pm.from as usize][pm.to as usize]);
    let mut scores: Vec<(i32, Move)> = moves
        .iter()
        .map(|&mv| {
            let mut s = 0i32;
            if Some(mv) == hash_move {
                s = 10_000_000;
            } else if let Some(promo) = mv.promotion {
                s = 900_000 + promo.value();
                if let Some(victim) = board.piece_at(mv.to) {
                    // promotion capture: victim value on top
                    s += victim.pt.value() * 10;
                }
            } else if let Some(victim) = board.piece_at(mv.to) {
                let attacker = board.piece_at(mv.from).map(|p| p.pt.value()).unwrap_or(0);
                // MVV-LVA: most valuable victim, least valuable attacker.
                s = 1_000_000 + victim.pt.value() * 10 - attacker;
            } else if Some(mv.to) == board.ep {
                // En passant: capture a pawn with a pawn.
                s = 1_000_000 + PieceType::Pawn.value() * 10 - PieceType::Pawn.value();
            } else {
                // Quiet: killers, then countermove, then history.
                if ply < killers.len() {
                    if killers[ply][0] == Some(mv) {
                        s = 800_000;
                    } else if killers[ply][1] == Some(mv) {
                        s = 790_000;
                    } else if counter_move == Some(mv) {
                        s = 785_000;
                    } else {
                        s = history[side.idx()][mv.from as usize][mv.to as usize];
                    }
                }
            }
            (s, mv)
        })
        .collect();
    scores.sort_by(|a, b| b.0.cmp(&a.0));
    for (i, (_, mv)) in scores.into_iter().enumerate() {
        moves[i] = mv;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_promos() -> Vec<Option<Move>> {
        let mut v = vec![
            None,
            Some(Move::new(0, 0, None)),
            Some(Move::new(12, 28, None)),
            Some(Move::new(63, 0, None)),
        ];
        for pt in [
            PieceType::Knight,
            PieceType::Bishop,
            PieceType::Rook,
            PieceType::Queen,
        ] {
            v.push(Some(Move::new(52, 60, Some(pt))));
        }
        v
    }

    #[test]
    fn tt_pack_round_trip() {
        let scores = [0, 1, -1, 150, -321, MATE - 7, -(MATE - 3), INF, -INF];
        for best in all_promos() {
            for &score in &scores {
                for &depth in &[0u8, 1, 6, 64, 255] {
                    for &flag in &[TT_EXACT, TT_LOWER, TT_UPPER] {
                        for &generation in &[0u16, 1, 255, 32768, 65535] {
                            let (b2, s2, d2, f2, g2) =
                                unpack_tt(pack_tt(best, score, depth, flag, generation));
                            assert_eq!(b2, best, "move {best:?} score {score}");
                            assert_eq!(s2, score);
                            assert_eq!(d2, depth);
                            assert_eq!(f2, flag);
                            assert_eq!(g2, generation);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn tt_probe_store_basics() {
        let tt = TT::new(1);
        // Zeroed table: everything (including key 0) misses.
        assert!(tt.probe(12345).is_none());
        assert!(tt.probe(0).is_none());
        // A genuine key-0 entry still verifies (SALT is nonzero).
        let mv = Move::new(12, 28, None);
        tt.store(0, Some(mv), 33, 4, TT_EXACT);
        let e = tt.probe(0).expect("key-0 entry must verify");
        assert_eq!(e.best, Some(mv));
        assert_eq!(e.score, 33);
        assert_eq!(e.depth, 4);
        assert_eq!(e.flag, TT_EXACT);
        // Missing key misses.
        assert!(tt.probe(999).is_none());
        // None best-move round-trips.
        tt.store(777, None, -50, 2, TT_UPPER);
        let e = tt.probe(777).expect("stored entry must hit");
        assert_eq!(e.best, None);
        assert_eq!(e.score, -50);
    }

    #[test]
    fn tt_depth_preferred_and_collision() {
        let tt = TT::new(1);
        let n = tt.slots.len() as u64;
        let m1 = Move::new(8, 16, None);
        let m2 = Move::new(9, 17, None);
        // Same key: deeper replaces, shallower does not.
        tt.store(42, Some(m1), 10, 5, TT_EXACT);
        tt.store(42, Some(m2), 20, 3, TT_EXACT);
        assert_eq!(tt.probe(42).unwrap().best, Some(m1));
        tt.store(42, Some(m2), 20, 7, TT_LOWER);
        let e = tt.probe(42).unwrap();
        assert_eq!(
            (e.best, e.score, e.depth, e.flag),
            (Some(m2), 20, 7, TT_LOWER)
        );
        // Collision (different key, same slot: keys differ by table length).
        tt.store(1000, Some(m1), 11, 9, TT_EXACT);
        tt.store(1000 + n, Some(m2), 22, 1, TT_EXACT);
        assert!(tt.probe(1000).is_none(), "evicted key must miss");
        assert_eq!(tt.probe(1000 + n).unwrap().best, Some(m2));
    }

    #[test]
    fn tt_generation_prefers_fresh() {
        // P2: a new search generation supersedes stale entries even when
        // shallower; within one generation depth still rules.
        let tt = TT::new(1);
        let m1 = Move::new(8, 16, None);
        let m2 = Move::new(9, 17, None);
        // Generation 0: deep entry stored.
        tt.store(42, Some(m1), 10, 9, TT_EXACT);
        assert_eq!(tt.probe(42).unwrap().best, Some(m1));
        // New search: shallow store wins over the stale deep entry.
        assert_eq!(tt.new_search(), 1);
        tt.store(42, Some(m2), 20, 1, TT_EXACT);
        let e = tt.probe(42).unwrap();
        assert_eq!((e.best, e.depth), (Some(m2), 1));
        // Same generation: deeper still replaces, shallower does not.
        tt.store(42, Some(m1), 10, 5, TT_EXACT);
        assert_eq!(tt.probe(42).unwrap().depth, 5);
        tt.store(42, Some(m2), 20, 3, TT_EXACT);
        assert_eq!(tt.probe(42).unwrap().depth, 5);
        // Clear resets to generation 0 with a clean slate (next search
        // is generation 1 again).
        tt.clear();
        assert!(tt.probe(42).is_none());
        assert_eq!(tt.new_search(), 1);
    }

    #[test]
    fn smp_search_returns_legal_move() {
        // Multi-threaded search must return a legal move. No exact-score
        // assertion: shared-TT timing makes tied depths nondeterministic.
        let mut board =
            crate::board::Board::from_fen(crate::board::STARTPOS_FEN).expect("valid startpos");
        let tt = TT::new(1);
        let lim = Limits {
            depth: Some(4),
            ..Default::default()
        };
        let mut no_poll = || false;
        let res = search_root(&mut board, &tt, &lim, 4, &mut no_poll);
        let mv = res.best.expect("must return a move");
        assert!(crate::movegen::generate_legal(&mut board).contains(&mv));
        assert!(res.nodes > 0);
        // Single-threaded path still works through the same entry point.
        let mut board2 =
            crate::board::Board::from_fen(crate::board::STARTPOS_FEN).expect("valid startpos");
        let res1 = search_root(&mut board2, &tt, &lim, 1, &mut no_poll);
        let mv1 = res1.best.expect("must return a move");
        assert!(crate::movegen::generate_legal(&mut board2).contains(&mv1));
    }

    #[test]
    fn tt_threaded_smoke() {
        let tt = TT::new(1);
        let n = tt.slots.len() as u64;
        assert!(n > 4000, "test needs distinct slots");
        std::thread::scope(|s| {
            for tid in 0..4u64 {
                let tt = &tt;
                s.spawn(move || {
                    for i in 0..1000u64 {
                        let key = tid * 1000 + i + 1; // distinct slots, nonzero
                        let mv = Move::new((i & 63) as u8, ((i + 7) & 63) as u8, None);
                        tt.store(key, Some(mv), i as i32 - 500, 6, TT_EXACT);
                        // Slots are never shared between writers, so our own
                        // store must read back coherently even mid-race.
                        let e = tt.probe(key).expect("own slot must hit");
                        assert_eq!(e.best, Some(mv));
                        assert_eq!(e.score, i as i32 - 500);
                    }
                });
            }
        });
        // No slot was shared between writers, so every key must hit exactly.
        for tid in 0..4u64 {
            for i in 0..1000u64 {
                let key = tid * 1000 + i + 1;
                let e = tt.probe(key).expect("undisturbed slot must hit");
                assert_eq!(e.score, i as i32 - 500);
                assert_eq!(e.depth, 6);
            }
        }
    }

    #[test]
    fn abort_leaves_board_intact() {
        // P0.1: every `make` must meet its `unmake` on ALL paths, including
        // abort. Let two consultations pass (covers a clean
        // between-iteration stop), then abort: the third consultation always
        // lands mid-iteration (a tick check at a 1024-node multiple), where
        // the unwound path must still unmake. Counting consultations (not
        // nodes) keeps this deterministic single-threaded.
        let tt = TT::new(1);
        let lim = Limits {
            depth: Some(20),
            ..Default::default()
        };
        for fen in [
            crate::board::STARTPOS_FEN,
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        ] {
            let mut board = crate::board::Board::from_fen(fen).expect("valid fen");
            let fen0 = board.to_fen();
            let consults = std::cell::Cell::new(0);
            let mut abort_mid_search = || {
                let n = consults.get() + 1;
                consults.set(n);
                n >= 3
            };
            let res = search_root(&mut board, &tt, &lim, 1, &mut abort_mid_search);
            // Aborted before completing depth 1: previous-iteration fallback
            // (root_moves[0]), still a legal move from the true position.
            let mv = res.best.expect("fallback move");
            assert!(crate::movegen::generate_legal(&mut board).contains(&mv));
            assert_eq!(board.to_fen(), fen0, "aborted search mutated the board");
        }
    }

    #[test]
    fn think_time_budgets() {
        // P1: pin the time math so future tuning can't silently break it.
        // Movetime uses a scale-aware margin (10% bounded to [30, 200]).
        let mt = |ms| Limits {
            movetime: Some(ms),
            ..Default::default()
        };
        assert_eq!(mt(150).think_time_ms(Color::White), Some(120));
        assert_eq!(mt(1000).think_time_ms(Color::White), Some(900));
        assert_eq!(mt(30).think_time_ms(Color::White), Some(10)); // floor
                                                                  // Clocks: own/20 + inc/2 with a scale-aware reserve.
        let clk = |w: u64, b: u64, wi: u64, bi: u64| Limits {
            wtime: Some(w),
            btime: Some(b),
            winc: Some(wi),
            binc: Some(bi),
            ..Default::default()
        };
        assert_eq!(
            clk(60000, 60000, 0, 0).think_time_ms(Color::White),
            Some(3000)
        );
        assert_eq!(
            clk(60000, 60000, 2000, 2000).think_time_ms(Color::Black),
            Some(4000)
        );
        // movestogo divisor.
        let mtg = Limits {
            wtime: Some(60000),
            movestogo: Some(10),
            ..Default::default()
        };
        assert_eq!(mtg.think_time_ms(Color::White), Some(6000));
        // No clock at all: untimed.
        assert_eq!(Limits::default().think_time_ms(Color::White), None);
        // Nearly-flagged clock still yields something playable.
        assert_eq!(clk(150, 150, 0, 0).think_time_ms(Color::White), Some(10));
    }

    #[test]
    fn tiny_movetime_returns_promptly() {
        // Soft/hard deadline machinery must answer fast and legally on a
        // near-zero budget, never hang past it by a wide margin.
        let tt = TT::new(1);
        let lim = Limits {
            movetime: Some(5),
            ..Default::default()
        };
        let mut board =
            crate::board::Board::from_fen(crate::board::STARTPOS_FEN).expect("valid startpos");
        let t = std::time::Instant::now();
        let res = search_root(&mut board, &tt, &lim, 1, &mut || false);
        let mv = res.best.expect("fallback move");
        assert!(crate::movegen::generate_legal(&mut board).contains(&mv));
        assert!(
            t.elapsed() < std::time::Duration::from_secs(5),
            "5ms search took {:?}",
            t.elapsed()
        );
    }
}
