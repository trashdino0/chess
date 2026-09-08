//! Core chess types: colors, pieces, moves, squares.
//! Square indexing: 0 = a1, 1 = b1, ..., 7 = h1, 8 = a2, ..., 63 = h8.

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum Color {
    White,
    Black,
}

impl Color {
    #[inline]
    pub fn opposite(self) -> Color {
        match self {
            Color::White => Color::Black,
            Color::Black => Color::White,
        }
    }

    #[inline]
    pub fn idx(self) -> usize {
        match self {
            Color::White => 0,
            Color::Black => 1,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub enum PieceType {
    Pawn,
    Knight,
    Bishop,
    Rook,
    Queen,
    King,
}

impl PieceType {
    #[inline]
    pub fn idx(self) -> usize {
        match self {
            PieceType::Pawn => 0,
            PieceType::Knight => 1,
            PieceType::Bishop => 2,
            PieceType::Rook => 3,
            PieceType::Queen => 4,
            PieceType::King => 5,
        }
    }

    /// Standard piece values in centipawns (king = 0, handled separately).
    #[inline]
    pub fn value(self) -> i32 {
        match self {
            PieceType::Pawn => 100,
            PieceType::Knight => 320,
            PieceType::Bishop => 330,
            PieceType::Rook => 500,
            PieceType::Queen => 900,
            PieceType::King => 0,
        }
    }

    pub fn from_promo_char(c: char) -> Option<PieceType> {
        match c {
            'n' => Some(PieceType::Knight),
            'b' => Some(PieceType::Bishop),
            'r' => Some(PieceType::Rook),
            'q' => Some(PieceType::Queen),
            _ => None,
        }
    }

    pub fn to_char(self) -> char {
        match self {
            PieceType::Pawn => 'p',
            PieceType::Knight => 'n',
            PieceType::Bishop => 'b',
            PieceType::Rook => 'r',
            PieceType::Queen => 'q',
            PieceType::King => 'k',
        }
    }

    pub fn from_char(c: char) -> Option<PieceType> {
        match c.to_ascii_lowercase() {
            'p' => Some(PieceType::Pawn),
            'n' => Some(PieceType::Knight),
            'b' => Some(PieceType::Bishop),
            'r' => Some(PieceType::Rook),
            'q' => Some(PieceType::Queen),
            'k' => Some(PieceType::King),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct Piece {
    pub pt: PieceType,
    pub color: Color,
}

impl Piece {
    pub fn from_char(c: char) -> Option<Piece> {
        let pt = PieceType::from_char(c)?;
        let color = if c.is_ascii_uppercase() {
            Color::White
        } else {
            Color::Black
        };
        Some(Piece { pt, color })
    }

    pub fn to_char(self) -> char {
        let c = self.pt.to_char();
        match self.color {
            Color::White => c.to_ascii_uppercase(),
            Color::Black => c,
        }
    }
}

/// A move. Special moves are inferred from the board state in `make_move`:
/// king moving two squares = castling, pawn moving to the EP square = EP
/// capture, pawn double push sets the EP square.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct Move {
    pub from: u8,
    pub to: u8,
    pub promotion: Option<PieceType>,
}

impl Move {
    #[inline]
    pub fn new(from: u8, to: u8, promotion: Option<PieceType>) -> Move {
        Move {
            from,
            to,
            promotion,
        }
    }

    /// Parse a UCI move string like "e2e4" or "e7e8q".
    pub fn from_uci(s: &str) -> Option<Move> {
        let b = s.as_bytes();
        if b.len() < 4 {
            return None;
        }
        let from = sq_from_name(&s[0..2])?;
        let to = sq_from_name(&s[2..4])?;
        let promotion = if b.len() >= 5 {
            Some(PieceType::from_promo_char(
                (b[4] as char).to_ascii_lowercase(),
            )?)
        } else {
            None
        };
        Some(Move {
            from,
            to,
            promotion,
        })
    }

    pub fn to_uci(self) -> String {
        let mut s = String::with_capacity(5);
        s.push_str(&sq_name(self.from));
        s.push_str(&sq_name(self.to));
        if let Some(p) = self.promotion {
            s.push(p.to_char());
        }
        s
    }
}

// Castling rights bitmask.
pub const WK: u8 = 1;
pub const WQ: u8 = 2;
pub const BK: u8 = 4;
pub const BQ: u8 = 8;

// Key squares.
pub const A1: u8 = 0;
pub const H1: u8 = 7;
pub const E1: u8 = 4;
pub const C1: u8 = 2;
pub const G1: u8 = 6;
pub const A8: u8 = 56;
pub const H8: u8 = 63;
pub const E8: u8 = 60;
pub const C8: u8 = 58;
pub const G8: u8 = 62;

#[inline]
pub fn file_of(sq: u8) -> u8 {
    sq & 7
}

#[inline]
pub fn rank_of(sq: u8) -> u8 {
    sq >> 3
}

#[inline]
pub fn make_sq(file: u8, rank: u8) -> u8 {
    rank * 8 + file
}

pub fn sq_name(sq: u8) -> String {
    let f = (b'a' + file_of(sq)) as char;
    let r = (b'1' + rank_of(sq)) as char;
    let mut s = String::with_capacity(2);
    s.push(f);
    s.push(r);
    s
}

pub fn sq_from_name(s: &str) -> Option<u8> {
    let b = s.as_bytes();
    if b.len() != 2 {
        return None;
    }
    if !(b'a'..=b'h').contains(&b[0]) || !(b'1'..=b'8').contains(&b[1]) {
        return None;
    }
    Some((b[1] - b'1') * 8 + (b[0] - b'a'))
}
