//! Negamax search with alpha-beta, iterative deepening, aspiration windows,
//! quiescence, transposition table, null-move pruning, LMR, check extensions,
//! killer + history move ordering and MVV-LVA capture ordering.

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
            return Some(mt.saturating_sub(30).max(10));
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
        if own > 200 {
            t = t.min(own - 100);
        }
        Some(t.clamp(10, 60_000 * 30))
    }
}

// ---------------------------------------------------------------------------
// Transposition table (fixed-size hash table, depth-preferred replacement)
// ---------------------------------------------------------------------------

#[derive(Copy, Clone)]
struct Entry {
    key: u64,
    best: Option<Move>,
    score: i32,
    depth: u8,
    flag: u8,
}

pub struct TT {
    slots: Vec<Option<Entry>>,
    mask: usize,
}

impl TT {
    pub fn new(mb: usize) -> TT {
        let bytes = (mb.max(1).min(1024) * 1024 * 1024) as u64;
        // Rough entry size; round slot count down to a power of two.
        let mut n = (bytes / 32).max(1024);
        n = n.next_power_of_two() >> 1;
        if n < 1024 {
            n = 1024;
        }
        TT {
            slots: vec![None; n as usize],
            mask: (n as usize) - 1,
        }
    }

    pub fn clear(&mut self) {
        self.slots.fill(None);
    }

    pub fn hashfull(&self) -> u32 {
        let sample = self.slots.len().min(1000);
        if sample == 0 {
            return 0;
        }
        let used = self
            .slots
            .iter()
            .take(sample)
            .filter(|e| e.is_some())
            .count();
        (used * 1000 / sample) as u32
    }

    fn probe(&self, key: u64) -> Option<Entry> {
        match self.slots[(key as usize) & self.mask] {
            Some(e) if e.key == key => Some(e),
            _ => None,
        }
    }

    fn store(&mut self, key: u64, best: Option<Move>, score: i32, depth: u8, flag: u8) {
        // Depth-preferred replacement: keep the deeper entry for the same
        // position so a shallow search can't evict valuable deep results.
        // Empty slots and key collisions (different position) always store.
        // (No generation aging yet: a per-search generation byte would mark
        // the previous move's still-useful entries stale, so age-aware
        // replacement is left for a later pass.)
        let idx = (key as usize) & self.mask;
        let replace = match self.slots[idx] {
            None => true,
            Some(e) if e.key != key => true,
            Some(e) => depth >= e.depth,
        };
        if replace {
            self.slots[idx] = Some(Entry {
                key,
                best,
                score,
                depth,
                flag,
            });
        }
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
pub fn search_root(
    board: &mut Board,
    tt: &mut TT,
    limits: &Limits,
    poll: &mut dyn FnMut() -> bool,
) -> SearchResult {
    let mut s = Searcher {
        tt,
        killers: [[None; 2]; MAX_PLY + 8],
        history: [[[0; 64]; 64]; 2],
        counter: [[None; 64]; 64],
        stack: [None; MAX_PLY + 32],
        nodes: 0,
        seldepth: 0,
        deadline: limits
            .think_time_ms(board.side)
            .map(|ms| Instant::now() + Duration::from_millis(ms)),
        max_depth: limits.max_depth(),
    };
    s.iterate(board, poll)
}

struct Searcher<'a> {
    tt: &'a mut TT,
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
}

impl Searcher<'_> {
    fn check_time(&self) -> bool {
        match self.deadline {
            Some(d) => Instant::now() >= d,
            None => false,
        }
    }

    #[inline]
    fn tick(&mut self, poll: &mut dyn FnMut() -> bool) -> Result<(), Abort> {
        if self.nodes & 1023 == 0 && (self.check_time() || poll()) {
            return Err(Abort);
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

        for depth in 1..=max_d {
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
            let sc = if i == 0 {
                -self.negamax(board, depth - 1, -beta, -alpha, 1, true, poll)?
            } else {
                // Principal variation search: null window first.
                let mut sc = -self.negamax(board, depth - 1, -alpha - 1, -alpha, 1, true, poll)?;
                if sc > alpha && sc < beta {
                    sc = -self.negamax(board, depth - 1, -beta, -alpha, 1, true, poll)?;
                }
                sc
            };
            board.unmake_move(undo);
            if sc > best_score {
                best_score = sc;
                best = mv;
            }
            if sc > alpha {
                alpha = sc;
            }
            if alpha >= beta {
                self.note_cutoff(board.side, mv, board, 0, depth);
                break;
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
            let sc = -self.negamax(board, depth - 1 - r, -beta, -beta + 1, ply + 1, false, poll)?;
            board.unmake_null(undo);
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
            let mut sc;
            if i >= 3 && depth >= 3 && !is_cap && !is_killer && !in_check {
                let r = if depth >= 6 && i >= 8 { 2 } else { 1 };
                sc = -self.negamax(
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
            } else if i == 0 {
                sc = -self.negamax(board, depth - 1, -beta, -alpha, ply + 1, true, poll)?;
            } else {
                sc = -self.negamax(board, depth - 1, -alpha - 1, -alpha, ply + 1, true, poll)?;
                if sc > alpha && sc < beta {
                    sc = -self.negamax(board, depth - 1, -beta, -alpha, ply + 1, true, poll)?;
                }
            }
            board.unmake_move(undo);

            if sc > best_score {
                best_score = sc;
                best_move = Some(mv);
            }
            if sc > alpha {
                alpha = sc;
            }
            if alpha >= beta {
                self.note_cutoff(board.side, mv, board, ply, depth);
                break;
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
            let sc = -self.quiescence(board, -beta, -alpha, ply + 1, poll)?;
            board.unmake_move(undo);
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

    fn print_info(&mut self, board: &mut Board, depth: u8, score: i32, start: &Instant) {
        let ms = start.elapsed().as_millis().max(1) as u64;
        let nps = self.nodes * 1000 / ms;
        let score_str = if score.abs() >= MATE_BOUND {
            let plies = MATE - score.abs();
            let moves = (plies + 1) / 2;
            format!("mate {}", if score > 0 { moves } else { -moves })
        } else {
            format!("cp {score}")
        };
        let pv = collect_pv(board, self.tt, depth as usize);
        let pv_str = pv.iter().map(|m| m.to_uci()).collect::<Vec<_>>().join(" ");
        println!(
            "info depth {} seldepth {} score {} nodes {} nps {} time {} hashfull {} pv {}",
            depth,
            self.seldepth,
            score_str,
            self.nodes,
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
