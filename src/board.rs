//! Board representation (per-color/per-piece bitboards plus occupancy),
//! FEN parsing, make/unmake, attack detection and draw detection.
//! Zobrist hashing included. (A mailbox `squares` array is still synced
//! alongside for direct lookup; it is removed in a later 3.1 sub-step.)

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::OnceLock;

use crate::bitboard::{
    bishop_attacks, bit, king_attacks, knight_attacks, pawn_attackers, rook_attacks,
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
    pub squares: [Option<Piece>; 64],
    /// Bitboards indexed `[color][piece]`; the source of truth for attacks
    /// and (later) move generation. Synced with `squares` on every make.
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
        let mut squares: [Option<Piece>; 64] = [None; 64];
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
                    squares[((rank as u8) * 8 + (file as u8)) as usize] = Some(p);
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
            squares,
            pieces: [[0; 6]; 2],
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
        for sq in 0..64u8 {
            if let Some(p) = b.squares[sq as usize] {
                b.pieces[p.color.idx()][p.pt.idx()] |= bit(sq);
            }
        }
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

    /// Debug/test helper: every mailbox square agrees with the bitboards,
    /// and occupancy matches the piece boards.
    #[cfg(test)]
    fn check_bb(&self) {
        for sq in 0..64u8 {
            let mut found = None;
            for c in 0..2 {
                for pt in 0..6 {
                    if self.pieces[c][pt] & bit(sq) != 0 {
                        assert!(found.is_none(), "square {sq} on two boards");
                        found = Some((c, pt));
                    }
                }
            }
            let expect = self.squares[sq as usize].map(|p| (p.color.idx(), p.pt.idx()));
            assert_eq!(found, expect, "bitboard/mailbox mismatch on {sq}");
        }
        for c in 0..2 {
            assert_eq!(
                self.occ[c],
                self.pieces[c].iter().fold(0u64, |a, bb| a | bb),
                "occ[{c}] stale"
            );
        }
        assert_eq!(self.occ_all, self.occ[0] | self.occ[1], "occ_all stale");
    }

    pub fn to_fen(&self) -> String {
        let mut s = String::new();
        for r in (0..8).rev() {
            let mut empty = 0;
            for f in 0..8 {
                match self.squares[(r * 8 + f) as usize] {
                    None => empty += 1,
                    Some(p) => {
                        if empty > 0 {
                            s.push(('0' as u8 + empty) as char);
                            empty = 0;
                        }
                        s.push(p.to_char());
                    }
                }
            }
            if empty > 0 {
                s.push(('0' as u8 + empty) as char);
            }
            if r > 0 {
                s.push('/');
            }
        }
        s.push(if self.side == Color::White { ' ' } else { ' ' });
        // rebuild properly:
        let mut out = s;
        out.pop(); // drop placeholder space handling, rewrite below
        out.push(' ');
        out.push(if self.side == Color::White { 'w' } else { 'b' });
        out.push(' ');
        if self.castling == 0 {
            out.push('-');
        } else {
            if self.castling & WK != 0 {
                out.push('K');
            }
            if self.castling & WQ != 0 {
                out.push('Q');
            }
            if self.castling & BK != 0 {
                out.push('k');
            }
            if self.castling & BQ != 0 {
                out.push('q');
            }
        }
        out.push(' ');
        match self.ep {
            None => out.push('-'),
            Some(sq) => out.push_str(&sq_name(sq)),
        }
        out.push_str(&format!(" {} {}", self.half, self.full));
        out
    }

    fn compute_hash(&self) -> u64 {
        let t = tables();
        let mut h: u64 = 0;
        for sq in 0..64 {
            if let Some(p) = self.squares[sq] {
                h ^= t.pieces[p.color.idx()][p.pt.idx()][sq];
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
        let mover = self.squares[mv.from as usize].expect("make_move: no piece");
        debug_assert_eq!(mover.color, side);

        // Detect capture (before moving), including en passant.
        let is_ep_capture = mover.pt == PieceType::Pawn
            && Some(mv.to) == self.ep
            && self.squares[mv.to as usize].is_none();
        let mut captured_sq = mv.to;
        if is_ep_capture {
            captured_sq = if side == Color::White {
                mv.to - 8
            } else {
                mv.to + 8
            };
        }
        undo.captured_sq = captured_sq;
        undo.captured = self.squares[captured_sq as usize];
        if is_ep_capture {
            debug_assert!(undo.captured.map_or(false, |p| p.pt == PieceType::Pawn));
        }

        // Reset EP; set later if double push.
        self.ep = None;

        // Move the piece (handle promotion).
        self.squares[mv.from as usize] = None;
        // Remove EP-captured pawn.
        if is_ep_capture {
            self.squares[captured_sq as usize] = None;
        }
        let placed = match mv.promotion {
            Some(pt) => Piece::new(pt, side),
            None => mover,
        };
        self.squares[mv.to as usize] = Some(placed);

        // Castling: king moved two squares -> move the rook.
        if mover.pt == PieceType::King && (file_of(mv.from) as i8 - file_of(mv.to) as i8).abs() == 2
        {
            match mv.to {
                x if x == G1 => {
                    self.squares[H1 as usize] = None;
                    self.squares[5] = Some(Piece::new(PieceType::Rook, side));
                }
                x if x == C1 => {
                    self.squares[A1 as usize] = None;
                    self.squares[3] = Some(Piece::new(PieceType::Rook, side));
                }
                x if x == G8 => {
                    self.squares[H8 as usize] = None;
                    self.squares[61] = Some(Piece::new(PieceType::Rook, side));
                }
                x if x == C8 => {
                    self.squares[A8 as usize] = None;
                    self.squares[59] = Some(Piece::new(PieceType::Rook, side));
                }
                _ => {}
            }
        }

        // Mirror the piece movement on the bitboards.
        {
            let si = side.idx();
            let ei = side.opposite().idx();
            self.pieces[si][mover.pt.idx()] &= !bit(mv.from);
            if let Some(cap) = undo.captured {
                self.pieces[ei][cap.pt.idx()] &= !bit(captured_sq);
            }
            let placed_pt = mv.promotion.unwrap_or(mover.pt);
            self.pieces[si][placed_pt.idx()] |= bit(mv.to);
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
                    let ri = PieceType::Rook.idx();
                    self.pieces[si][ri] &= !bit(rf);
                    self.pieces[si][ri] |= bit(rt);
                }
            }
            self.refresh_occupancy();
        }

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

        self.hash = self.compute_hash();
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

        // Undo castling rook move.
        let mover = self.squares[mv.to as usize].expect("unmake: no piece");
        if mover.pt == PieceType::King && (file_of(mv.from) as i8 - file_of(mv.to) as i8).abs() == 2
        {
            match mv.to {
                x if x == G1 => {
                    self.squares[5] = None;
                    self.squares[H1 as usize] = Some(Piece::new(PieceType::Rook, self.side));
                }
                x if x == C1 => {
                    self.squares[3] = None;
                    self.squares[A1 as usize] = Some(Piece::new(PieceType::Rook, self.side));
                }
                x if x == G8 => {
                    self.squares[61] = None;
                    self.squares[H8 as usize] = Some(Piece::new(PieceType::Rook, self.side));
                }
                x if x == C8 => {
                    self.squares[59] = None;
                    self.squares[A8 as usize] = Some(Piece::new(PieceType::Rook, self.side));
                }
                _ => {}
            }
        }

        // Restore mover (un-promote back to pawn).
        let orig = if undo.mv.unwrap().promotion.is_some() {
            Piece::new(PieceType::Pawn, self.side)
        } else {
            mover
        };
        self.squares[mv.from as usize] = Some(orig);
        self.squares[mv.to as usize] = None;
        // Restore captured piece (EP goes on its own square).
        if let Some(cap) = undo.captured {
            self.squares[undo.captured_sq as usize] = Some(cap);
        }

        // Mirror the restoration on the bitboards (`mover`/`orig`/`cap`
        // above carry the pre-move piece identities).
        {
            let si = self.side.idx();
            let ei = self.side.opposite().idx();
            self.pieces[si][mover.pt.idx()] &= !bit(mv.to);
            if let Some(cap) = undo.captured {
                self.pieces[ei][cap.pt.idx()] |= bit(undo.captured_sq);
            }
            self.pieces[si][orig.pt.idx()] |= bit(mv.from);
            if mover.pt == PieceType::King
                && (file_of(mv.from) as i8 - file_of(mv.to) as i8).abs() == 2
            {
                let (rf, rt) = match mv.to {
                    x if x == G1 => (5, H1),
                    x if x == C1 => (3, A1),
                    x if x == G8 => (61, H8),
                    x if x == C8 => (59, A8),
                    _ => (0, 0),
                };
                if rf != rt {
                    let ri = PieceType::Rook.idx();
                    self.pieces[si][ri] &= !bit(rf);
                    self.pieces[si][ri] |= bit(rt);
                }
            }
            self.refresh_occupancy();
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
        self.hash = self.compute_hash();
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
                let c = match self.squares[(r * 8 + f) as usize] {
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
    fn bitboards_track_mailbox() {
        // Walk the perft tree; at every node the bitboards must agree with
        // the mailbox on all 64 squares (catches make/unmake sync bugs).
        fn walk(b: &mut Board, depth: u32) {
            b.check_bb();
            if depth == 0 {
                return;
            }
            for mv in crate::movegen::generate_legal(b) {
                let u = b.make_move(mv);
                walk(b, depth - 1);
                b.unmake_move(u);
                b.check_bb();
            }
        }
        for fen in [
            STARTPOS_FEN,
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
        ] {
            let mut b = Board::from_fen(fen).unwrap();
            walk(&mut b, 3);
        }
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
        let mut undos = Vec::new();
        // A capture and its reply (victim present on the target square).
        for _ in 0..2 {
            let cap = generate_legal(&mut b)
                .into_iter()
                .find(|m| b.squares[m.to as usize].is_some())
                .unwrap();
            undos.push(b.make_move(cap));
        }
        undos.push(b.make_null());
        // Castling when available (drops rights), else a quiet.
        let castle = generate_legal(&mut b).into_iter().find(|m| {
            b.squares[m.from as usize].is_some_and(|p| p.pt == PieceType::King)
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
