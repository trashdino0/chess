//! Board representation (per-color/per-piece bitboards plus occupancy),
//! FEN parsing, make/unmake, attack detection and draw detection.
//! Zobrist hashing included.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::OnceLock;

use crate::bitboard::{
    bishop_attacks, bit, king_attacks, knight_attacks, pawn_attackers, pop_lsb, rook_attacks,
};
use crate::types::*;

/// Identity hasher for pre-hashed `u64` keys (Zobrist hashes): std's
/// default SipHash would dominate the cost of the O(1) repetition lookup,
/// so map operations use the key directly as the hash.
#[derive(Default)]
struct IdentityHasher(u64);

impl Hasher for IdentityHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.wrapping_mul(31).wrapping_add(b as u64);
        }
    }
    #[inline]
    fn write_u64(&mut self, v: u64) {
        self.0 = v;
    }
}

type HashCounts = HashMap<u64, u32, BuildHasherDefault<IdentityHasher>>;

// ---------------------------------------------------------------------------
// Zobrist hashing (deterministic SplitMix64, no external crates)
// ---------------------------------------------------------------------------

struct Tables {
    pieces: [[[u64; 64]; 6]; 2],
    side: u64,
    castling: [u64; 16],
    ep_file: [u64; 8],
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut seed: u64 = 0x243F6A8885A308D3;
        let mut t = Tables {
            pieces: [[[0; 64]; 6]; 2],
            side: 0,
            castling: [0; 16],
            ep_file: [0; 8],
        };
        for c in 0..2 {
            for p in 0..6 {
                for s in 0..64 {
                    t.pieces[c][p][s] = splitmix64(&mut seed);
                }
            }
        }
        t.side = splitmix64(&mut seed);
        for i in 0..16 {
            t.castling[i] = splitmix64(&mut seed);
        }
        for i in 0..8 {
            t.ep_file[i] = splitmix64(&mut seed);
        }
        t
    })
}

// ---------------------------------------------------------------------------
// Undo
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Undo {
    pub mv: Option<Move>,
    pub captured: Option<Piece>,
    pub captured_sq: u8,
    pub castling: u8,
    pub ep: Option<u8>,
    pub half: u32,
    pub hash: u64,
}

// ---------------------------------------------------------------------------
// Board
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Board {
    /// Bitboards indexed `[color][piece type]`; the sole piece placement
    /// store. Everything else (occupancy, hash, history) derives from it.
    pub pieces: [[u64; 6]; 2],
    /// Occupancy per color, and combined. Rebuilt from `pieces` on make.
    pub occ: [u64; 2],
    pub occ_all: u64,
    pub side: Color,
    pub castling: u8,
    pub ep: Option<u8>,
    pub half: u32,
    pub full: u32,
    pub hash: u64,
    /// Hash of every position reached (initial + after each make, incl. nulls).
    pub hash_history: Vec<u64>,
    /// Occurrence count per hash in `hash_history`, kept in sync by
    /// make/unmake (incl. nulls) so repetition checks are O(1).
    hash_counts: HashCounts,
}

impl Board {
    /// Piece on `sq`, if any (occupancy fast path, then one type scan).
    pub fn piece_at(&self, sq: u8) -> Option<Piece> {
        let b = bit(sq);
        if self.occ_all & b == 0 {
            return None;
        }
        let ci = if self.occ[0] & b != 0 { 0 } else { 1 };
        let color = if ci == 0 { Color::White } else { Color::Black };
        let boards = &self.pieces[ci];
        // PieceType::idx order: Pawn = 0, Knight = 1, Bishop = 2,
        // Rook = 3, Queen = 4, King = 5.
        let pt = if boards[0] & b != 0 {
            PieceType::Pawn
        } else if boards[1] & b != 0 {
            PieceType::Knight
        } else if boards[2] & b != 0 {
            PieceType::Bishop
        } else if boards[3] & b != 0 {
            PieceType::Rook
        } else if boards[4] & b != 0 {
            PieceType::Queen
        } else {
            debug_assert!(boards[5] & b != 0, "occ set but no piece on {sq}");
            PieceType::King
        };
        Some(Piece { pt, color })
    }
}

pub const STARTPOS_FEN: &str = "rnbqkbnr/pppppppp/8/8/8/8/PPPPPPPP/RNBQKBNR w KQkq - 0 1";

impl Board {
    pub fn new_startpos() -> Board {
        Board::from_fen(STARTPOS_FEN).expect("valid startpos")
    }

    pub fn from_fen(fen: &str) -> Result<Board, String> {
        let parts: Vec<&str> = fen.split_whitespace().collect();
        if parts.len() < 4 {
            return Err(format!("FEN needs at least 4 fields: {fen}"));
        }
        let mut pieces: [[u64; 6]; 2] = [[0; 6]; 2];
        let mut rank: i8 = 7;
        let mut file: i8 = 0;
        for ch in parts[0].chars() {
            match ch {
                '/' => {
                    if file != 8 {
                        return Err("FEN: bad rank width".to_string());
                    }
                    rank -= 1;
                    file = 0;
                    if rank < 0 {
                        return Err("FEN: too many ranks".to_string());
                    }
                }
                '1'..='8' => {
                    file += (ch as i8) - ('0' as i8);
                    if file > 8 {
                        return Err("FEN: rank overflow".to_string());
                    }
                }
                _ => {
                    let p = Piece::from_char(ch)
                        .ok_or_else(|| format!("FEN: bad piece char '{ch}'"))?;
                    if file >= 8 || rank < 0 {
                        return Err("FEN: square overflow".to_string());
                    }
                    let sq = (rank as u8) * 8 + (file as u8);
                    pieces[p.color.idx()][p.pt.idx()] |= bit(sq);
                    file += 1;
                }
            }
        }
        if rank != 0 || file != 8 {
            return Err("FEN: incomplete board".to_string());
        }

        let side = match parts[1] {
            "w" => Color::White,
            "b" => Color::Black,
            _ => return Err("FEN: side must be w/b".to_string()),
        };

        let mut castling: u8 = 0;
        if parts[2] != "-" {
            for ch in parts[2].chars() {
                match ch {
                    'K' => castling |= WK,
                    'Q' => castling |= WQ,
                    'k' => castling |= BK,
                    'q' => castling |= BQ,
                    _ => return Err(format!("FEN: bad castling char '{ch}'")),
                }
            }
        }

        let ep = if parts[3] == "-" {
            None
        } else {
            Some(sq_from_name(parts[3]).ok_or("FEN: bad EP square")?)
        };

        let half = if parts.len() > 4 {
            parts[4].parse::<u32>().map_err(|_| "FEN: bad halfmove")?
        } else {
            0
        };
        let full = if parts.len() > 5 {
            parts[5].parse::<u32>().map_err(|_| "FEN: bad fullmove")?
        } else {
            1
        };

        let mut b = Board {
            pieces,
            occ: [0; 2],
            occ_all: 0,
            side,
            castling,
            ep,
            half,
            full,
            hash: 0,
            hash_history: Vec::new(),
            hash_counts: HashCounts::default(),
        };
        b.refresh_occupancy();
        b.hash = b.compute_hash();
        b.hash_history.push(b.hash);
        b.hash_counts.insert(b.hash, 1);
        Ok(b)
    }

    /// Rebuild per-color and combined occupancy from the piece bitboards.
    fn refresh_occupancy(&mut self) {
        for c in 0..2 {
            self.occ[c] = self.pieces[c].iter().fold(0u64, |a, bb| a | bb);
        }
        self.occ_all = self.occ[0] | self.occ[1];
    }

    /// Test helper: occupancy matches the piece boards, and no square is on
    /// two piece boards at once.
    /// (The make/unmake cross-checks below inline the fold instead: a
    /// #[cfg(test)] helper is unresolvable inside release `debug_assert!`.)
    #[cfg(test)]
    fn check_occ(&self) {
        let mut all = 0u64;
        for c in 0..2 {
            for pt in 0..6 {
                assert!(all & self.pieces[c][pt] == 0, "square on two boards");
                all |= self.pieces[c][pt];
            }
            assert_eq!(
                self.occ[c],
                self.pieces[c].iter().fold(0u64, |a, bb| a | bb),
                "occ[{c}] stale"
            );
        }
        assert_eq!(self.occ_all, self.occ[0] | self.occ[1], "occ_all stale");
        assert_eq!(self.occ_all, all, "occ_all disagrees with pieces");
    }

    pub fn to_fen(&self) -> String {
        let mut s = String::new();
        for r in (0..8).rev() {
            let mut empty = 0;
            for f in 0..8 {
                match self.piece_at(r * 8 + f) {
                    None => empty += 1,
                    Some(p) => {
                        if empty > 0 {
                            s.push((b'0' + empty) as char);
                            empty = 0;
                        }
                        s.push(p.to_char());
                    }
                }
            }
            if empty > 0 {
                s.push((b'0' + empty) as char);
            }
            if r > 0 {
                s.push('/');
            }
        }
        s.push(' ');
        s.push(if self.side == Color::White { 'w' } else { 'b' });
        s.push(' ');
        if self.castling == 0 {
            s.push('-');
        } else {
            if self.castling & WK != 0 {
                s.push('K');
            }
            if self.castling & WQ != 0 {
                s.push('Q');
            }
            if self.castling & BK != 0 {
                s.push('k');
            }
            if self.castling & BQ != 0 {
                s.push('q');
            }
        }
        s.push(' ');
        match self.ep {
            None => s.push('-'),
            Some(sq) => s.push_str(&sq_name(sq)),
        }
        s.push_str(&format!(" {} {}", self.half, self.full));
        s
    }

    fn compute_hash(&self) -> u64 {
        let t = tables();
        let mut h: u64 = 0;
        for c in 0..2 {
            for pt in 0..6 {
                let mut bb = self.pieces[c][pt];
                while bb != 0 {
                    h ^= t.pieces[c][pt][pop_lsb(&mut bb) as usize];
                }
            }
        }
        if self.side == Color::Black {
            h ^= t.side;
        }
        h ^= t.castling[self.castling as usize];
        if let Some(ep) = self.ep {
            h ^= t.ep_file[file_of(ep) as usize];
        }
        h
    }

    pub fn king_sq(&self, color: Color) -> Option<u8> {
        let bb = self.pieces[color.idx()][PieceType::King.idx()];
        if bb == 0 {
            None
        } else {
            // Lowest set bit = first king in 0..64 scan order (same as before).
            Some(bb.trailing_zeros() as u8)
        }
    }

    /// Is `sq` attacked by side `by`? (Bitboard version: leapers/pawns are
    /// table ops, sliders walk occupancy rays.)
    pub fn is_square_attacked(&self, sq: u8, by: Color) -> bool {
        let bi = by.idx();
        // Pawns.
        if self.pieces[bi][PieceType::Pawn.idx()] & pawn_attackers(sq, by) != 0 {
            return true;
        }
        // Knights, king.
        if self.pieces[bi][PieceType::Knight.idx()] & knight_attacks(sq) != 0 {
            return true;
        }
        if self.pieces[bi][PieceType::King.idx()] & king_attacks(sq) != 0 {
            return true;
        }
        // Sliding pieces: diagonals (bishop/queen), orthogonals (rook/queen).
        let diag =
            self.pieces[bi][PieceType::Bishop.idx()] | self.pieces[bi][PieceType::Queen.idx()];
        if bishop_attacks(sq, self.occ_all) & diag != 0 {
            return true;
        }
        let orth = self.pieces[bi][PieceType::Rook.idx()] | self.pieces[bi][PieceType::Queen.idx()];
        if rook_attacks(sq, self.occ_all) & orth != 0 {
            return true;
        }
        false
    }

    pub fn in_check(&self, color: Color) -> bool {
        match self.king_sq(color) {
            Some(k) => self.is_square_attacked(k, color.opposite()),
            None => false,
        }
    }

    fn clear_castling_on_square(&mut self, sq: u8) {
        match sq {
            x if x == E1 => self.castling &= !(WK | WQ),
            x if x == E8 => self.castling &= !(BK | BQ),
            x if x == A1 => self.castling &= !WQ,
            x if x == H1 => self.castling &= !WK,
            x if x == A8 => self.castling &= !BQ,
            x if x == H8 => self.castling &= !BK,
            _ => {}
        }
    }

    /// Make a move on the board, returning undo info. Assumes pseudo-legal move.
    pub fn make_move(&mut self, mv: Move) -> Undo {
        let undo = Undo {
            mv: Some(mv),
            captured: None,
            captured_sq: mv.to,
            castling: self.castling,
            ep: self.ep,
            half: self.half,
            hash: self.hash,
        };
        let mut undo = undo;

        let side = self.side;
        let si = side.idx();
        let mover = self.piece_at(mv.from).expect("make_move: no piece");
        debug_assert_eq!(mover.color, side);

        // Detect capture (before moving), including en passant.
        let is_ep_capture =
            mover.pt == PieceType::Pawn && Some(mv.to) == self.ep && self.occ_all & bit(mv.to) == 0;
        let mut captured_sq = mv.to;
        if is_ep_capture {
            captured_sq = if side == Color::White {
                mv.to - 8
            } else {
                mv.to + 8
            };
        }
        undo.captured_sq = captured_sq;
        undo.captured = self.piece_at(captured_sq);
        if is_ep_capture {
            debug_assert!(undo.captured.is_some_and(|p| p.pt == PieceType::Pawn));
        }

        // Reset EP; set later if double push.
        self.ep = None;

        // Move the piece bits (handle promotion).
        self.pieces[si][mover.pt.idx()] &= !bit(mv.from);
        if let Some(cap) = undo.captured {
            // Clear by recorded owner, not assumed enemy: for every
            // generated capture this IS the enemy, and a phantom EP passed
            // in by hand stays make/unmake symmetric instead of corrupting.
            self.pieces[cap.color.idx()][cap.pt.idx()] &= !bit(captured_sq);
        }
        let placed_pt = mv.promotion.unwrap_or(mover.pt);
        self.pieces[si][placed_pt.idx()] |= bit(mv.to);

        // Castling: king moved two squares -> move the rook bits.
        // Record the rook relocation for the incremental hash below.
        let mut rook_move = None;
        if mover.pt == PieceType::King && (file_of(mv.from) as i8 - file_of(mv.to) as i8).abs() == 2
        {
            let (rf, rt) = match mv.to {
                x if x == G1 => (H1, 5),
                x if x == C1 => (A1, 3),
                x if x == G8 => (H8, 61),
                x if x == C8 => (A8, 59),
                _ => (0, 0),
            };
            if rf != rt {
                let ri = PieceType::Rook.idx();
                self.pieces[si][ri] &= !bit(rf);
                self.pieces[si][ri] |= bit(rt);
                rook_move = Some((rf, rt));
            }
        }
        // Incremental occupancy: toggle exactly the squares that changed
        // instead of re-folding all 12 piece bitboards (O(1) vs O(pieces)
        // per make — same P8.1 pattern as the Zobrist hash, identical values
        // by construction). Victim traps mirror the hash: a phantom
        // self-capture (captured_sq == from, e.g. a push onto a stale EP
        // square) must skip the victim toggle or it re-sets the vacated
        // square, and the combined board only gains the jumped square when
        // it differs from both `from` and `to` (normal captures land ON the
        // victim: no extra bit). The debug_assert cross-checks every make
        // in dev/test builds; perft suites exercise millions of positions.
        self.occ[si] ^= bit(mv.from) ^ bit(mv.to);
        if let Some(cap) = undo.captured && captured_sq != mv.from {
            self.occ[cap.color.idx()] ^= bit(captured_sq);
        }
        // Combined board: `from` always vacates. `to` gains the mover
        // except on a normal capture (victim-out + mover-in cancel on the
        // same square); the EP jumped square vacates when it differs.
        self.occ_all ^= bit(mv.from);
        match undo.captured {
            None => self.occ_all ^= bit(mv.to),
            Some(_) if captured_sq == mv.to => {}
            Some(_) => {
                self.occ_all ^= bit(mv.to);
                if captured_sq != mv.from {
                    self.occ_all ^= bit(captured_sq);
                }
            }
        }
        if let Some((rf, rt)) = rook_move {
            self.occ[si] ^= bit(rf) ^ bit(rt);
            self.occ_all ^= bit(rf) ^ bit(rt);
        }
        debug_assert_eq!(
            self.occ[0],
            self.pieces[0].iter().fold(0u64, |a, bb| a | bb)
        );
        debug_assert_eq!(
            self.occ[1],
            self.pieces[1].iter().fold(0u64, |a, bb| a | bb)
        );
        debug_assert_eq!(self.occ_all, self.occ[0] | self.occ[1]);

        // Pawn double push -> set EP square.
        if mover.pt == PieceType::Pawn && (rank_of(mv.from) as i8 - rank_of(mv.to) as i8).abs() == 2
        {
            self.ep = Some(if side == Color::White {
                mv.from + 8
            } else {
                mv.from - 8
            });
        }

        // Halfmove clock.
        if mover.pt == PieceType::Pawn || undo.captured.is_some() {
            self.half = 0;
        } else {
            self.half += 1;
        }

        // Castling rights.
        self.clear_castling_on_square(mv.from);
        self.clear_castling_on_square(captured_sq);

        // Fullmove + side flip.
        if side == Color::Black {
            self.full += 1;
        }
        self.side = side.opposite();

        // Incremental Zobrist: toggle exactly what changed instead of
        // re-XORing all ~32 pieces (O(1) vs O(pieces) per make). Values are
        // identical to compute_hash by construction — XOR order is
        // irrelevant, and stale EP squares round-trip because old/new keys
        // come from the recorded `undo.ep` / final `self.ep` respectively.
        // The debug_assert cross-checks every make in dev/test builds
        // (perft suites alone verify millions of positions); release pays
        // only the handful of XORs.
        let t = tables();
        let mut h = self.hash;
        // EP files share one key per file (not per square), so a move that
        // replaces one EP square with another on the SAME file (e.g. pawn
        // d7d6 while a stale d6 lingers) must toggle only on difference:
        // removing and re-adding the same key would cancel to zero while
        // the recompute holds it exactly once.
        let old_ep = undo.ep.map(|ep| file_of(ep) as usize);
        let new_ep = self.ep.map(|ep| file_of(ep) as usize);
        if old_ep != new_ep {
            if let Some(f) = old_ep {
                h ^= t.ep_file[f];
            }
            if let Some(f) = new_ep {
                h ^= t.ep_file[f];
            }
        }
        h ^= t.castling[undo.castling as usize];
        h ^= t.pieces[si][mover.pt.idx()][mv.from as usize];
        h ^= t.pieces[si][placed_pt.idx()][mv.to as usize];
        if let Some(cap) = undo.captured {
            // A hand-fed phantom EP can "capture" the mover's own origin
            // square (victim key == mover key); toggling twice would cancel
            // while the recompute holds it zero times (d7 vacated). Skip
            // the victim toggle then — generated moves never hit this
            // (EP victims sit beside `to`, never on `from`).
            if captured_sq != mv.from {
                h ^= t.pieces[cap.color.idx()][cap.pt.idx()][captured_sq as usize];
            }
        }
        if let Some((rf, rt)) = rook_move {
            let ri = PieceType::Rook.idx();
            h ^= t.pieces[si][ri][rf as usize];
            h ^= t.pieces[si][ri][rt as usize];
        }
        h ^= t.castling[self.castling as usize];
        h ^= t.side;
        self.hash = h;
        debug_assert_eq!(self.hash, self.compute_hash());
        self.hash_history.push(self.hash);
        *self.hash_counts.entry(self.hash).or_insert(0) += 1;
        undo
    }

    pub fn unmake_move(&mut self, undo: Undo) {
        let mv = undo.mv.expect("unmake of null via unmake_move");
        // Pop current hash, restore.
        self.hash_history.pop();
        self.decrement_count(self.hash);
        self.side = self.side.opposite();
        if self.side == Color::Black {
            self.full -= 1;
        }
        self.castling = undo.castling;
        self.ep = undo.ep;
        self.half = undo.half;
        self.hash = undo.hash;

        // The piece currently on `to` is what was placed by make_move
        // (promotion piece or the mover itself).
        let mover = self.piece_at(mv.to).expect("unmake: no piece");

        // Undo castling rook move.
        if mover.pt == PieceType::King && (file_of(mv.from) as i8 - file_of(mv.to) as i8).abs() == 2
        {
            let si = self.side.idx();
            let ri = PieceType::Rook.idx();
            match mv.to {
                x if x == G1 => {
                    self.pieces[si][ri] &= !bit(5);
                    self.pieces[si][ri] |= bit(H1);
                }
                x if x == C1 => {
                    self.pieces[si][ri] &= !bit(3);
                    self.pieces[si][ri] |= bit(A1);
                }
                x if x == G8 => {
                    self.pieces[si][ri] &= !bit(61);
                    self.pieces[si][ri] |= bit(H8);
                }
                x if x == C8 => {
                    self.pieces[si][ri] &= !bit(59);
                    self.pieces[si][ri] |= bit(A8);
                }
                _ => {}
            }
        }

        // Restore mover (un-promote back to pawn) and captured piece
        // (EP goes on its own square).
        {
            let si = self.side.idx();
            self.pieces[si][mover.pt.idx()] &= !bit(mv.to);
            if let Some(cap) = undo.captured {
                // Restore by recorded owner (see make_move): symmetric even
                // for a phantom EP handed in directly.
                self.pieces[cap.color.idx()][cap.pt.idx()] |= bit(undo.captured_sq);
            }
            let orig_pt = if mv.promotion.is_some() {
                PieceType::Pawn
            } else {
                mover.pt
            };
            self.pieces[si][orig_pt.idx()] |= bit(mv.from);
            // Incremental occupancy: XOR is self-inverse, so unmake applies
            // the same toggles as make (same trap guards). Castling rook
            // relocation reversed in the same XOR form.
            self.occ[si] ^= bit(mv.from) ^ bit(mv.to);
            if let Some(cap) = undo.captured && undo.captured_sq != mv.from {
                self.occ[cap.color.idx()] ^= bit(undo.captured_sq);
            }
            self.occ_all ^= bit(mv.from);
            match undo.captured {
                None => self.occ_all ^= bit(mv.to),
                Some(_) if undo.captured_sq == mv.to => {}
                Some(_) => {
                    self.occ_all ^= bit(mv.to);
                    if undo.captured_sq != mv.from {
                        self.occ_all ^= bit(undo.captured_sq);
                    }
                }
            }
            if mover.pt == PieceType::King
                && (file_of(mv.from) as i8 - file_of(mv.to) as i8).abs() == 2
            {
                let (rf, rt) = match mv.to {
                    x if x == G1 => (H1, 5),
                    x if x == C1 => (A1, 3),
                    x if x == G8 => (H8, 61),
                    x if x == C8 => (A8, 59),
                    _ => (0, 0),
                };
                if rf != rt {
                    self.occ[si] ^= bit(rf) ^ bit(rt);
                    self.occ_all ^= bit(rf) ^ bit(rt);
                }
            }
            debug_assert_eq!(
                self.occ[0],
                self.pieces[0].iter().fold(0u64, |a, bb| a | bb)
            );
            debug_assert_eq!(
                self.occ[1],
                self.pieces[1].iter().fold(0u64, |a, bb| a | bb)
            );
            debug_assert_eq!(self.occ_all, self.occ[0] | self.occ[1]);
        }
    }

    /// Null move: pass the turn (used in null-move pruning).
    pub fn make_null(&mut self) -> Undo {
        let undo = Undo {
            mv: None,
            captured: None,
            captured_sq: 0,
            castling: self.castling,
            ep: self.ep,
            half: self.half,
            hash: self.hash,
        };
        self.ep = None;
        self.half += 1;
        if self.side == Color::Black {
            self.full += 1;
        }
        self.side = self.side.opposite();
        // Incremental Zobrist (same scheme as make_move): drop the old EP
        // key (a null move always clears EP) and flip the side key.
        // Castling rights never change under a null move, so no-op there.
        let t = tables();
        let mut h = self.hash;
        if let Some(ep) = undo.ep {
            h ^= t.ep_file[file_of(ep) as usize];
        }
        h ^= t.side;
        self.hash = h;
        debug_assert_eq!(self.hash, self.compute_hash());
        self.hash_history.push(self.hash);
        *self.hash_counts.entry(self.hash).or_insert(0) += 1;
        undo
    }

    pub fn unmake_null(&mut self, undo: Undo) {
        self.hash_history.pop();
        self.decrement_count(self.hash);
        self.side = self.side.opposite();
        if self.side == Color::Black {
            self.full -= 1;
        }
        self.ep = undo.ep;
        self.half = undo.half;
        self.hash = undo.hash;
    }

    /// Decrement the occurrence count of a hash being left via unmake.
    /// Must be called with the current (about-to-be-popped) hash, before
    /// `self.hash` is restored from the undo.
    fn decrement_count(&mut self, hash: u64) {
        match self.hash_counts.get_mut(&hash) {
            Some(c) => {
                *c -= 1;
                if *c == 0 {
                    self.hash_counts.remove(&hash);
                }
            }
            None => debug_assert!(false, "hash_counts out of sync in unmake"),
        }
    }

    /// Occurrences of the current position hash in history (O(1)).
    pub fn hash_count(&self) -> usize {
        self.hash_counts.get(&self.hash).copied().unwrap_or(0) as usize
    }

    pub fn is_fifty_move(&self) -> bool {
        self.half >= 100
    }

    /// K vs K or K+minor vs K.
    pub fn is_insufficient_material(&self) -> bool {
        // Any pawn, rook or queen on either side: sufficient.
        for c in 0..2 {
            if self.pieces[c][PieceType::Pawn.idx()]
                | self.pieces[c][PieceType::Rook.idx()]
                | self.pieces[c][PieceType::Queen.idx()]
                != 0
            {
                return false;
            }
        }
        let minors = (self.pieces[0][PieceType::Knight.idx()]
            | self.pieces[0][PieceType::Bishop.idx()]
            | self.pieces[1][PieceType::Knight.idx()]
            | self.pieces[1][PieceType::Bishop.idx()])
        .count_ones();
        minors <= 1
    }

    pub fn is_draw(&self) -> bool {
        self.is_fifty_move() || self.is_insufficient_material() || self.hash_count() >= 3
    }

    /// Simple ASCII board for the `d` debug command.
    pub fn pretty(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("FEN: {}\n", self.to_fen()));
        for r in (0..8).rev() {
            s.push_str(&format!("{} ", r + 1));
            for f in 0..8 {
                let c = match self.piece_at(r * 8 + f) {
                    None => '.',
                    Some(p) => p.to_char(),
                };
                s.push(c);
                s.push(' ');
            }
            s.push('\n');
        }
        s.push_str("  a b c d e f g h\n");
        s.push_str(&format!(
            "Side: {:?}  Castling: {}  EP: {}  half: {} full: {}  drawn: {}\n",
            self.side,
            castling_str(self.castling),
            self.ep.map(sq_name).unwrap_or_else(|| "-".to_string()),
            self.half,
            self.full,
            self.is_draw()
        ));
        s
    }
}

fn castling_str(c: u8) -> String {
    if c == 0 {
        return "-".to_string();
    }
    let mut s = String::new();
    if c & WK != 0 {
        s.push('K');
    }
    if c & WQ != 0 {
        s.push('Q');
    }
    if c & BK != 0 {
        s.push('k');
    }
    if c & BQ != 0 {
        s.push('q');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn play(b: &mut Board, uci: &str) -> Undo {
        let mv = Move::from_uci(uci).unwrap();
        b.make_move(mv)
    }

    #[test]
    fn incremental_hash_matches_full_recompute() {
        // P8: tricky sequence — double push (sets EP), EP-adjacent play,
        // capture, promotion, castling (moves the rook), null move. After
        // every step the incremental hash must equal a from-scratch
        // recompute; unmakes restore by construction (undo.hash).
        let mut b =
            Board::from_fen("r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1")
                .unwrap();
        assert_eq!(b.hash, b.compute_hash());
        // a2a4 double push sets an EP square (hash must include it).
        let u1 = play(&mut b, "a2a4");
        assert_eq!(b.hash, b.compute_hash());
        assert_eq!(b.ep, Move::from_uci("a2a3").map(|m| m.to));
        // ...c7c5 replies with its own double push (old EP out, new in).
        let u2 = play(&mut b, "c7c5");
        assert_eq!(b.hash, b.compute_hash());
        assert_eq!(b.ep, Move::from_uci("c7c6").map(|m| m.to));
        // Same-file EP replacement: stale d6 lingers while d7d6 is played
        // (EP keys are per-file, so remove+add of file d must NOT cancel).
        let mut s =
            Board::from_fen("r1bqkbnr/pppp1ppp/2n5/4p3/3PP3/5N2/PPP2PPP/RNBQKB1R b KQkq d6 0 1")
                .unwrap();
        let us = play(&mut s, "d7d6");
        assert_eq!(s.hash, s.compute_hash());
        assert_eq!(s.ep, None);
        s.unmake_move(us);
        assert_eq!(s.hash, s.compute_hash());
        // Capture on e4-side tension square if available, else any capture.
        use crate::movegen::generate_legal;
        if let Some(cap) = generate_legal(&mut b)
            .into_iter()
            .find(|m| b.piece_at(m.to).is_some())
        {
            let _u = b.make_move(cap);
            assert_eq!(b.hash, b.compute_hash());
            b.unmake_move(_u);
            assert_eq!(b.hash, b.compute_hash());
        }
        // Promotion race position: white pawn one step from queening.
        let mut p = Board::from_fen("4k3/2P5/8/8/8/8/1K6/8 w - - 0 1").unwrap();
        let up = play(&mut p, "c7c8q");
        assert_eq!(p.hash, p.compute_hash());
        p.unmake_move(up);
        assert_eq!(p.hash, p.compute_hash());
        // Kingside castle moves king + rook and drops both rights.
        let mut c = Board::from_fen("r3k2r/pppppppp/8/8/8/8/PPPPPPPP/R3K2R w KQkq - 0 1").unwrap();
        let uc = play(&mut c, "e1g1");
        assert_eq!(c.hash, c.compute_hash());
        c.unmake_move(uc);
        assert_eq!(c.hash, c.compute_hash());
        // Null move flips side + clears EP.
        let un = b.make_null();
        assert_eq!(b.hash, b.compute_hash());
        b.unmake_null(un);
        assert_eq!(b.hash, b.compute_hash());
        // Unwind the whole line: exact restoration.
        b.unmake_move(u2);
        b.unmake_move(u1);
        assert_eq!(b.hash, b.compute_hash());
    }

    #[test]
    fn make_unmake_round_trips_state() {
        // Sequence covering captures, a null move, castling (rights change)
        // and plain quiets; moves come from the legal set so the sequence
        // is always valid.
        use crate::movegen::generate_legal;
        let mut b =
            Board::from_fen("r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1")
                .unwrap();
        let fen0 = b.to_fen();
        let (hash0, count0, hist0, map0) = (
            b.hash,
            b.hash_count(),
            b.hash_history.len(),
            b.hash_counts.len(),
        );
        let occ0 = b.occ_all.count_ones();
        let mut undos = Vec::new();
        // A capture and its reply (victim present on the target square).
        for _ in 0..2 {
            let cap = generate_legal(&mut b)
                .into_iter()
                .find(|m| b.piece_at(m.to).is_some())
                .unwrap();
            undos.push(b.make_move(cap));
        }
        undos.push(b.make_null());
        // Castling when available (drops rights), else a quiet.
        let castle = generate_legal(&mut b).into_iter().find(|m| {
            b.piece_at(m.from).is_some_and(|p| p.pt == PieceType::King)
                && (file_of(m.from) as i8 - file_of(m.to) as i8).abs() == 2
        });
        match castle {
            Some(mv) => undos.push(b.make_move(mv)),
            None => {
                let q = generate_legal(&mut b)[0];
                undos.push(b.make_move(q));
            }
        }
        for _ in 0..4 {
            let mv = generate_legal(&mut b)[0];
            undos.push(b.make_move(mv));
        }
        while let Some(u) = undos.pop() {
            match u.mv {
                Some(_) => b.unmake_move(u),
                None => b.unmake_null(u),
            }
        }
        assert_eq!(b.to_fen(), fen0);
        assert_eq!(b.hash, hash0);
        assert_eq!(b.hash_count(), count0);
        assert_eq!(b.hash_history.len(), hist0);
        assert_eq!(b.hash_counts.len(), map0);
        // Occupancy integrity: same pieces before and after.
        b.check_occ();
        assert_eq!(b.occ_all.count_ones(), occ0);

        // Promotion round-trip.
        let mut p = Board::from_fen("8/2P2k2/8/8/8/8/5K2/8 w - - 0 1").unwrap();
        let pfen = p.to_fen();
        let u = play(&mut p, "c7c8q");
        assert_eq!(p.hash_history.len(), 2);
        p.unmake_move(u);
        assert_eq!(p.to_fen(), pfen);
        assert_eq!(p.hash_history.len(), 1);
        assert_eq!(p.hash_counts.len(), 1);
    }

    #[test]
    fn stale_ep_square_emits_no_phantom_capture() {
        // Match regression (13 illegal `d7e8q` losses): a FEN with a stale
        // EP square (d6 set, no white pawn on d5) emitted phantom Black EP
        // captures (c7xd6/e7xd6); their make/unmake restored the "captured"
        // own pawn to the wrong color (+d7 white pawn), poisoning the board
        // until search played a king capture. Generation must emit nothing
        // targeting a victimless EP square, and filtering must not mutate.
        use crate::movegen::{generate_legal, generate_pseudo};
        let mut b =
            Board::from_fen("r1bqkbnr/pppp1ppp/2n5/4p3/3PP3/5N2/PPP2PPP/RNBQKB1R b KQkq d6 0 1")
                .unwrap();
        let fen0 = b.to_fen();
        // The phantom EP captures (c7/e7xd6 with no white pawn on d5) are
        // gone, while the ordinary quiet push d7d6 is still generated.
        let phantom1 = Move::from_uci("c7d6").unwrap();
        let phantom2 = Move::from_uci("e7d6").unwrap();
        let quiet_push = Move::from_uci("d7d6").unwrap();
        assert!(!generate_pseudo(&b).contains(&phantom1));
        assert!(!generate_pseudo(&b).contains(&phantom2));
        assert!(generate_pseudo(&b).contains(&quiet_push));
        assert!(!generate_legal(&mut b).contains(&phantom1));
        assert!(!generate_legal(&mut b).contains(&phantom2));
        assert_eq!(b.to_fen(), fen0, "legality filtering mutated the board");
        // The full match scenario: e5xd4 applies with no phantom pawn.
        let u = play(&mut b, "e5d4");
        assert_eq!(
            b.to_fen(),
            "r1bqkbnr/pppp1ppp/2n5/8/3pP3/5N2/PPP2PPP/RNBQKB1R w KQkq - 0 2"
        );
        b.unmake_move(u);
        assert_eq!(b.to_fen(), fen0);
        // A hand-fed phantom EP (c7->d6 while ep=d6) round-trips exactly:
        // the own-pawn "victim" is restored to its recorded owner.
        let u = play(&mut b, "c7d6");
        b.unmake_move(u);
        assert_eq!(b.to_fen(), fen0);
        b.check_occ();
        // A real EP capture still generates and round-trips (d5 victim
        // present): e5xd6 removes the d5 pawn.
        let mut e = Board::from_fen("4k3/8/8/3pP3/8/8/8/4K3 w - d6 0 1").unwrap();
        let ep_mv = Move::from_uci("e5d6").unwrap();
        assert!(generate_pseudo(&e).contains(&ep_mv));
        let efen = e.to_fen();
        let u = play(&mut e, "e5d6");
        assert_eq!(e.to_fen(), "4k3/8/3P4/8/8/8/8/4K3 b - - 0 1");
        e.unmake_move(u);
        assert_eq!(e.to_fen(), efen);
    }

    #[test]
    fn repetition_counts_incremental() {
        let mut b = Board::new_startpos();
        assert_eq!(b.hash_count(), 1);
        // Knight dance out and back: startpos occurs twice.
        for m in ["g1f3", "g8f6", "f3g1", "f6g8"] {
            play(&mut b, m);
        }
        assert_eq!(b.hash_count(), 2);
        assert!(!b.is_draw());
        for m in ["g1f3", "g8f6", "f3g1", "f6g8"] {
            play(&mut b, m);
        }
        assert_eq!(b.hash_count(), 3);
        assert!(b.is_draw());
        // Null moves participate in history and round-trip too.
        let n = b.make_null();
        assert_eq!(b.hash_history.len(), 10);
        b.unmake_null(n);
        assert_eq!(b.hash_count(), 3);
        assert_eq!(b.hash_history.len(), 9);
    }
}
