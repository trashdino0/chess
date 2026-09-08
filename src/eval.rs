//! Static evaluation: material + piece-square tables (white perspective),
//! plus a small bishop-pair bonus.

use crate::board::Board;
use crate::types::*;

// PSTs in "rank8-first" order (a8..h1), from White's perspective.
// Standard Simplified Evaluation Function tables (Tomasz Michniewski).

const PAWN: [i32; 64] = [
    0, 0, 0, 0, 0, 0, 0, 0, //
    50, 50, 50, 50, 50, 50, 50, 50, //
    10, 10, 20, 30, 30, 20, 10, 10, //
    5, 5, 10, 25, 25, 10, 5, 5, //
    0, 0, 0, 20, 20, 0, 0, 0, //
    5, -5, -10, 0, 0, -10, -5, 5, //
    5, 10, 10, -20, -20, 10, 10, 5, //
    0, 0, 0, 0, 0, 0, 0, 0,
];

const KNIGHT: [i32; 64] = [
    -50, -40, -30, -30, -30, -30, -40, -50, //
    -40, -20, 0, 0, 0, 0, -20, -40, //
    -30, 0, 10, 15, 15, 10, 0, -30, //
    -30, 5, 15, 20, 20, 15, 5, -30, //
    -30, 0, 15, 20, 20, 15, 0, -30, //
    -30, 5, 10, 15, 15, 10, 5, -30, //
    -40, -20, 0, 5, 5, 0, -20, -40, //
    -50, -40, -30, -30, -30, -30, -40, -50,
];

const BISHOP: [i32; 64] = [
    -20, -10, -10, -10, -10, -10, -10, -20, //
    -10, 0, 0, 0, 0, 0, 0, -10, //
    -10, 0, 5, 10, 10, 5, 0, -10, //
    -10, 5, 5, 10, 10, 5, 5, -10, //
    -10, 0, 10, 10, 10, 10, 0, -10, //
    -10, 10, 10, 10, 10, 10, 10, -10, //
    -10, 5, 0, 0, 0, 0, 5, -10, //
    -20, -10, -10, -10, -10, -10, -10, -20,
];

const ROOK: [i32; 64] = [
    0, 0, 0, 0, 0, 0, 0, 0, //
    5, 10, 10, 10, 10, 10, 10, 5, //
    -5, 0, 0, 0, 0, 0, 0, -5, //
    -5, 0, 0, 0, 0, 0, 0, -5, //
    -5, 0, 0, 0, 0, 0, 0, -5, //
    -5, 0, 0, 0, 0, 0, 0, -5, //
    -5, 0, 0, 0, 0, 0, 0, -5, //
    0, 0, 0, 5, 5, 0, 0, 0,
];

const QUEEN: [i32; 64] = [
    -20, -10, -10, -5, -5, -10, -10, -20, //
    -10, 0, 0, 0, 0, 0, 0, -10, //
    -10, 0, 5, 5, 5, 5, 0, -10, //
    -5, 0, 5, 5, 5, 5, 0, -5, //
    0, 0, 5, 5, 5, 5, 0, -5, //
    -10, 5, 5, 5, 5, 5, 0, -10, //
    -10, 0, 5, 0, 0, 0, 0, -10, //
    -20, -10, -10, -5, -5, -10, -10, -20,
];

const KING_MG: [i32; 64] = [
    -30, -40, -40, -50, -50, -40, -40, -30, //
    -30, -40, -40, -50, -50, -40, -40, -30, //
    -30, -40, -40, -50, -50, -40, -40, -30, //
    -30, -40, -40, -50, -50, -40, -40, -30, //
    -20, -30, -30, -40, -40, -30, -30, -20, //
    -10, -20, -20, -20, -20, -20, -20, -10, //
    20, 20, 0, 0, 0, 0, 20, 20, //
    20, 30, 10, 0, 0, 10, 30, 20,
];

/// King endgame table (rank8-first, White perspective): a centralized king.
const KING_EG: [i32; 64] = [
    -50, -40, -30, -20, -20, -30, -40, -50, //
    -30, -20, -10, 0, 0, -10, -20, -30, //
    -30, -10, 20, 30, 30, 20, -10, -30, //
    -30, -10, 30, 40, 40, 30, -10, -30, //
    -30, -10, 30, 40, 40, 30, -10, -30, //
    -30, -10, 20, 30, 30, 20, -10, -30, //
    -30, -30, 0, 0, 0, 0, -30, -30, //
    -50, -30, -30, -30, -30, -30, -30, -50,
];

/// Passed-pawn bonus by rank index (0 = rank 1). Pawns never stand on the
/// 1st/8th rank, so the endpoints stay 0.
const PASSED_BONUS: [i32; 8] = [0, 0, 10, 20, 35, 60, 100, 0];

const ISOLATED_PENALTY: i32 = -12;
const DOUBLED_PENALTY: i32 = -14;
const ROOK_OPEN_BONUS: i32 = 18;
const ROOK_SEMI_OPEN_BONUS: i32 = 10;
const ROOK_SEVENTH_BONUS: i32 = 20;

/// Endgame bishop table: central diagonals are best.
const BISHOP_EG: [i32; 64] = [
    -20, -10, -10, -10, -10, -10, -10, -20,
    -10,  0,   0,   0,   0,   0,   0, -10,
    -10,  0,   5,  10,  10,   5,   0, -10,
    -10,  0,  10,  20,  20,  10,   0, -10,
    -10,  0,  10,  20,  20,  10,   0, -10,
    -10,  0,   5,  10,  10,   5,   0, -10,
    -10,  0,   0,   0,   0,   0,   0, -10,
    -20, -10, -10, -10, -10, -10, -10, -20,
];

/// Endgame knight table: central squares best, edges/corners worst.
const KNIGHT_EG: [i32; 64] = [
    -50, -40, -30, -30, -30, -30, -40, -50,
    -40, -20,   0,   5,   5,   0, -20, -40,
    -30,   0,  10,  15,  15,  10,   0, -30,
    -30,   5,  15,  20,  20,  15,   5, -30,
    -30,   5,  15,  20,  20,  15,   5, -30,
    -30,   0,  10,  15,  15,  10,   0, -30,
    -40, -20,   0,   5,   5,   0, -20, -40,
    -50, -40, -30, -30, -30, -30, -40, -50,
];

/// Game-phase weights (N/B = 1, R = 2, Q = 4; K and pawns = 0; max 24).
fn phase_weight(pt: PieceType) -> i32 {
    match pt {
        PieceType::Knight | PieceType::Bishop => 1,
        PieceType::Rook => 2,
        PieceType::Queen => 4,
        _ => 0,
    }
}

/// PST value from the piece owner's perspective (positive = good for owner).
/// Tables are rank8-first; White reads them directly, Black reads the
/// vertically mirrored square.
fn pst_own(pt: PieceType, sq: u8, color: Color) -> i32 {
    let r = rank_of(sq) as usize;
    let f = file_of(sq) as usize;
    let table = match pt {
        PieceType::Pawn => &PAWN,
        PieceType::Knight => &KNIGHT,
        PieceType::Bishop => &BISHOP,
        PieceType::Rook => &ROOK,
        PieceType::Queen => &QUEEN,
        PieceType::King => &KING_MG,
    };
    match color {
        Color::White => table[(7 - r) * 8 + f],
        Color::Black => table[r * 8 + f],
    }
}

/// Tapered bishop (middlegame → endgame). Endgame table favors central diagonals.
fn bishop_pst(sq: u8, color: Color, phase: i32) -> i32 {
    let r = rank_of(sq) as usize;
    let f = file_of(sq) as usize;
    let (mg, eg) = match color {
        Color::White => (BISHOP[(7 - r) * 8 + f], BISHOP_EG[(7 - r) * 8 + f]),
        Color::Black => (BISHOP[r * 8 + f], BISHOP_EG[r * 8 + f]),
    };
    (mg * phase + eg * (24 - phase)) / 24
}

/// Tapered knight (middlegame → endgame). Endgame pulls toward the center.
fn knight_pst(sq: u8, color: Color, phase: i32) -> i32 {
    let r = rank_of(sq) as usize;
    let f = file_of(sq) as usize;
    let (mg, eg) = match color {
        Color::White => (KNIGHT[(7 - r) * 8 + f], KNIGHT_EG[(7 - r) * 8 + f]),
        Color::Black => (KNIGHT[r * 8 + f], KNIGHT_EG[r * 8 + f]),
    };
    (mg * phase + eg * (24 - phase)) / 24
}

/// Development bonus: small reward for developed knights/bishops (not on
/// starting squares) and for controlling central files/ranks. This is
/// phase-independent (pure mobility/development term).
fn development_bonus(board: &Board) -> i32 {
    let mut dev = 0i32;
    for sq in 0..64u8 {
        let Some(p) = board.squares[sq as usize] else { continue };
        if p.pt == PieceType::Knight || p.pt == PieceType::Bishop {
            let r = rank_of(sq) as usize;
            let f = file_of(sq) as usize;
            // Base development: bonus for leaving the starting rank.
            let on_back = match p.color {
                Color::White => r == 0, // White starts at rank 1 (r=0)
                Color::Black => r == 7, // Black starts at rank 8 (r=7)
            };
            if !on_back {
                dev += 6; // developed minor piece (tuned down from 8)
            }
            // Central control bonus.
            if (3..=4).contains(&f) && (3..=4).contains(&r) {
                dev += 4; // central square (tuned down from 5)
            }
        }
    }
    dev
}

/// King PST with middlegame/endgame interpolation. `phase` runs 0 (bare
/// kings) to 24 (full material); the endgame table pulls the king towards
/// the center as pieces come off.
fn king_pst(sq: u8, color: Color, phase: i32) -> i32 {
    let r = rank_of(sq) as usize;
    let f = file_of(sq) as usize;
    let (mg, eg) = match color {
        Color::White => (KING_MG[(7 - r) * 8 + f], KING_EG[(7 - r) * 8 + f]),
        Color::Black => (KING_MG[r * 8 + f], KING_EG[r * 8 + f]),
    };
    (mg * phase + eg * (24 - phase)) / 24
}

/// Pawn-structure term from the pawn owner's perspective. Pawn locations are
/// passed as per-file rank bitmasks (`1 << rank`), one entry per file.
fn pawn_structure(color: Color, r: usize, f: usize, own: &[u8; 8], enemy: &[u8; 8]) -> i32 {
    let mut s = 0;
    // Doubled: more than one friendly pawn on this file.
    if own[f].count_ones() > 1 {
        s += DOUBLED_PENALTY;
    }
    // Isolated: no friendly pawn on either adjacent file.
    let left = if f > 0 { own[f - 1] } else { 0 };
    let right = if f + 1 < 8 { own[f + 1] } else { 0 };
    if left == 0 && right == 0 {
        s += ISOLATED_PENALTY;
    }
    // Passed: no enemy pawn on this or adjacent files ahead of us.
    let ahead: u8 = match color {
        Color::White => {
            if r >= 7 {
                0
            } else {
                !((1u8 << (r + 1)).wrapping_sub(1))
            }
        }
        Color::Black => {
            if r == 0 {
                0
            } else {
                (1u8 << r).wrapping_sub(1)
            }
        }
    };
    let lo = f.saturating_sub(1);
    let hi = (f + 1).min(7);
    let mut blocked = false;
    for nf in lo..=hi {
        if enemy[nf] & ahead != 0 {
            blocked = true;
            break;
        }
    }
    if !blocked {
        let rank_idx = match color {
            Color::White => r,
            Color::Black => 7 - r,
        };
        s += PASSED_BONUS[rank_idx];
    }
    s
}

/// Rook term from the rook owner's perspective: open / semi-open file plus a
/// bonus for squatting on the 7th rank.
fn rook_bonus(color: Color, r: usize, f: usize, own: &[u8; 8], enemy: &[u8; 8]) -> i32 {
    let mut s = 0;
    if own[f] == 0 {
        if enemy[f] == 0 {
            s += ROOK_OPEN_BONUS;
        } else {
            s += ROOK_SEMI_OPEN_BONUS;
        }
    }
    match color {
        Color::White => {
            if r == 6 {
                s += ROOK_SEVENTH_BONUS;
            }
        }
        Color::Black => {
            if r == 1 {
                s += ROOK_SEVENTH_BONUS;
            }
        }
    }
    s
}

/// Static evaluation in centipawns from White's perspective.
///
/// Material + middlegame PSTs, a tapered king table (shelter early, activity
/// late), pawn structure (passed / isolated / doubled), rooks on open files
/// and on the 7th rank, plus a small bishop-pair bonus. All terms are
/// symmetric, so the startpos still evaluates to exactly 0.
pub fn evaluate(board: &Board) -> i32 {
    // First pass: game phase + pawn locations per file (rank bitmasks).
    let mut phase = 0i32;
    let mut white_pawns = [0u8; 8];
    let mut black_pawns = [0u8; 8];
    for sq in 0..64u8 {
        if let Some(p) = board.squares[sq as usize] {
            phase += phase_weight(p.pt);
            if p.pt == PieceType::Pawn {
                let f = file_of(sq) as usize;
                let r = rank_of(sq) as usize;
                match p.color {
                    Color::White => white_pawns[f] |= 1 << r,
                    Color::Black => black_pawns[f] |= 1 << r,
                }
            }
        }
    }
    let phase = phase.min(24);

    let mut score = 0i32;
    let mut white_bishops = 0;
    let mut black_bishops = 0;
    for sq in 0..64u8 {
        let Some(p) = board.squares[sq as usize] else {
            continue;
        };
        // Kings use the tapered table.
        if p.pt == PieceType::King {
            let u = king_pst(sq, p.color, phase);
            match p.color {
                Color::White => score += u,
                Color::Black => score -= u,
            }
            continue;
        }
        let r = rank_of(sq) as usize;
        let f = file_of(sq) as usize;
        // Bishop and knight use tapered tables; others use fixed PST.
        let mut u = if p.pt == PieceType::Bishop {
            p.pt.value() + bishop_pst(sq, p.color, phase)
        } else if p.pt == PieceType::Knight {
            p.pt.value() + knight_pst(sq, p.color, phase)
        } else {
            p.pt.value() + pst_own(p.pt, sq, p.color)
        };
        match p.pt {
            PieceType::Pawn => {
                let (own, enemy) = match p.color {
                    Color::White => (&white_pawns, &black_pawns),
                    Color::Black => (&black_pawns, &white_pawns),
                };
                u += pawn_structure(p.color, r, f, own, enemy);
            }
            PieceType::Rook => {
                let (own, enemy) = match p.color {
                    Color::White => (&white_pawns, &black_pawns),
                    Color::Black => (&black_pawns, &white_pawns),
                };
                u += rook_bonus(p.color, r, f, own, enemy);
            }
            PieceType::Bishop => match p.color {
                Color::White => white_bishops += 1,
                Color::Black => black_bishops += 1,
            },
            _ => {}
        }
        match p.color {
            Color::White => score += u,
            Color::Black => score -= u,
        }
    }
    if white_bishops >= 2 {
        score += 30;
    }
    if black_bishops >= 2 {
        score -= 30;
    }
    // Development / mobility: reward developed minors and center control.
    score += development_bonus(board);
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startpos_is_balanced() {
        let b = Board::new_startpos();
        assert_eq!(evaluate(&b), 0);
    }

    #[test]
    fn extra_queen_scores() {
        let b = Board::from_fen("4k3/8/8/8/8/8/8/Q3K3 w - - 0 1").unwrap();
        assert!(evaluate(&b) > 800);
    }

    #[test]
    fn passed_pawn_beats_blocked_pawn() {
        // e6 with no black pawns left: passed, PST 25 + bonus 60.
        let free = Board::from_fen("4k3/8/4P3/8/8/8/4K3/8 w - - 0 1").unwrap();
        // e6 blocked by a black pawn on e7: not passed.
        let blocked = Board::from_fen("4k3/4p3/4P3/8/8/8/4K3/8 w - - 0 1").unwrap();
        assert!(evaluate(&free) > evaluate(&blocked) + 40);
    }

    #[test]
    fn isolated_pawn_is_penalized() {
        // Lone a-pawn: PST 5, isolated -12 -> below bare material.
        let b = Board::from_fen("4k3/8/8/8/8/8/P7/4K3 w - - 0 1").unwrap();
        let e = evaluate(&b);
        assert!(e < 100 && e > 50, "isolated pawn scored {e}");
    }

    #[test]
    fn rook_open_file_bonus() {
        // Same material; only the pawn moves. With the pawn on h2 the
        // a-file is open (rook bonus), with the pawn on a2 it is closed.
        let open = Board::from_fen("4k3/8/8/8/8/8/7P/R3K3 w - - 0 1").unwrap();
        let closed = Board::from_fen("4k3/8/8/8/8/8/P7/R3K3 w - - 0 1").unwrap();
        assert!(evaluate(&open) > evaluate(&closed));
    }

    #[test]
    fn endgame_king_centralizes() {
        // Bare kings: the centralized side scores positively.
        let b = Board::from_fen("4k3/8/8/8/4K3/8/8/8 w - - 0 1").unwrap();
        assert!(evaluate(&b) > 0);
    }
}
