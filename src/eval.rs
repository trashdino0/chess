//! Static evaluation: material + piece-square tables (white perspective),
//! plus a small bishop-pair bonus.

use crate::bitboard::pop_lsb;
use crate::board::Board;
use crate::movegen::mobility_count;
use crate::types::*;

const PIECE_TYPES: [PieceType; 6] = [
    PieceType::Pawn,
    PieceType::Knight,
    PieceType::Bishop,
    PieceType::Rook,
    PieceType::Queen,
    PieceType::King,
];

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

/// Side-to-move bonus: moving first is worth roughly a tenth of a pawn.
/// Applied in `evaluate` (not `side_eval`) so the `eval` UCI command shows
/// it; symmetry tests must mirror the side to move as well as the pieces.
const TEMPO: i32 = 12;

/// Mobility weights in centipawns per pseudo-legal destination
/// (knights/bishops/rooks/queens only; pawns and kings are covered by
/// structure/safety terms). Kept small: mobility guides, never dominates.
const MOB_KNIGHT: i32 = 2;
const MOB_BISHOP: i32 = 2;
const MOB_ROOK: i32 = 2;
const MOB_QUEEN: i32 = 1;

fn mobility_weight(pt: PieceType) -> i32 {
    match pt {
        PieceType::Knight => MOB_KNIGHT,
        PieceType::Bishop => MOB_BISHOP,
        PieceType::Rook => MOB_ROOK,
        PieceType::Queen => MOB_QUEEN,
        _ => 0,
    }
}

const ISOLATED_PENALTY: i32 = -12;
const DOUBLED_PENALTY: i32 = -14;
const ROOK_OPEN_BONUS: i32 = 18;
const ROOK_SEMI_OPEN_BONUS: i32 = 10;
const ROOK_SEVENTH_BONUS: i32 = 20;

/// Endgame bishop table: central diagonals are best.
const BISHOP_EG: [i32; 64] = [
    -20, -10, -10, -10, -10, -10, -10, -20, -10, 0, 0, 0, 0, 0, 0, -10, -10, 0, 5, 10, 10, 5, 0,
    -10, -10, 0, 10, 20, 20, 10, 0, -10, -10, 0, 10, 20, 20, 10, 0, -10, -10, 0, 5, 10, 10, 5, 0,
    -10, -10, 0, 0, 0, 0, 0, 0, -10, -20, -10, -10, -10, -10, -10, -10, -20,
];

/// Endgame knight table: central squares best, edges/corners worst.
const KNIGHT_EG: [i32; 64] = [
    -50, -40, -30, -30, -30, -30, -40, -50, -40, -20, 0, 5, 5, 0, -20, -40, -30, 0, 10, 15, 15, 10,
    0, -30, -30, 5, 15, 20, 20, 15, 5, -30, -30, 5, 15, 20, 20, 15, 5, -30, -30, 0, 10, 15, 15, 10,
    0, -30, -40, -20, 0, 5, 5, 0, -20, -40, -50, -40, -30, -30, -30, -30, -40, -50,
];

/// Endgame pawn table (rank8-first, White perspective): advanced (central)
/// pawns are worth more with few pieces left; unadvanced flank pawns are
/// slight liabilities.
const PAWN_EG: [i32; 64] = [
    0, 0, 0, 0, 0, 0, 0, 0, //
    10, 10, 10, 10, 10, 10, 10, 10, //
    20, 20, 20, 20, 20, 20, 20, 20, //
    15, 15, 25, 30, 30, 25, 15, 15, //
    5, 5, 15, 25, 25, 15, 5, 5, //
    0, 0, 5, 15, 15, 5, 0, 0, //
    -5, -5, -5, 0, 0, -5, -5, -5, //
    0, 0, 0, 0, 0, 0, 0, 0,
];

/// Endgame rook table (rank8-first, White perspective): centralize the rook
/// once the board empties; 7th-rank activity still scores.
const ROOK_EG: [i32; 64] = [
    0, 0, 0, 5, 5, 0, 0, 0, //
    -5, 0, 0, 0, 0, 0, 0, -5, //
    -5, 0, 0, 0, 0, 0, 0, -5, //
    -5, 0, 0, 0, 0, 0, 0, -5, //
    -5, 0, 0, 0, 0, 0, 0, -5, //
    -5, 0, 0, 0, 0, 0, 0, -5, //
    5, 10, 10, 10, 10, 10, 10, 5, //
    0, 0, 0, 5, 5, 0, 0, 0,
];

/// Endgame queen table (rank8-first, White perspective): mild centralization
/// vs the middlegame table.
const QUEEN_EG: [i32; 64] = [
    -20, -10, -10, -5, -5, -10, -10, -20, //
    -10, 0, 5, 0, 0, 0, 0, -10, //
    -10, 5, 5, 5, 5, 5, 0, -10, //
    0, 5, 5, 5, 5, 5, 0, -5, //
    0, 0, 5, 5, 5, 5, 0, -5, //
    -10, 0, 5, 5, 5, 5, 0, -10, //
    -10, 0, 0, 0, 0, 0, 0, -10, //
    -20, -10, -10, -5, -5, -10, -10, -20,
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

/// Pawn-shield score for one king (unscaled): own pawns on the three files
/// around the king, one rank ahead (+8) or two ranks ahead (+4, still some
/// cover). Only while the king is still home (back two ranks); a
/// centralised endgame king has no shield to speak of. Cheap: 6 probes.
/// King-square lookup wrapper for tests (`evaluate` uses the tracked square
/// via `king_shield_sq` directly).
#[cfg(test)]
fn king_shield(board: &Board, color: Color) -> i32 {
    match board.king_sq(color) {
        Some(ksq) => king_shield_sq(board, ksq, color),
        None => 0,
    }
}

/// Shield for a known king square (hot path: `evaluate` tracks kings in its
/// first pass instead of re-scanning the board via `king_sq`).
fn king_shield_sq(board: &Board, ksq: u8, color: Color) -> i32 {
    let (kf, kr) = (file_of(ksq) as i8, rank_of(ksq) as i8);
    let (home, dir) = match color {
        Color::White => (kr <= 1, 1i8),
        Color::Black => (kr >= 6, -1i8),
    };
    if !home {
        return 0;
    }
    let mut s = 0;
    for df in -1..=1 {
        let f = kf + df;
        if !(0..8).contains(&f) {
            continue;
        }
        // kr is home-ranked, so r1/r2 stay on the board.
        let r1 = (kr + dir) as u8;
        let r2 = (kr + 2 * dir) as u8;
        if board
            .piece_at(make_sq(f as u8, r1))
            .is_some_and(|p| p.pt == PieceType::Pawn && p.color == color)
        {
            s += 8;
        } else if board
            .piece_at(make_sq(f as u8, r2))
            .is_some_and(|p| p.pt == PieceType::Pawn && p.color == color)
        {
            s += 4;
        }
    }
    s
}

/// Development bonus from White's perspective: small reward for developed
/// knights/bishops (not on starting squares) and for controlling central
/// files/ranks. Signed by owner color (a developed black piece is good for
/// Black). This is phase-independent (pure mobility/development term).
fn development_bonus(board: &Board) -> i32 {
    let mut dev = 0i32;
    for color in [Color::White, Color::Black] {
        for pt in [PieceType::Knight, PieceType::Bishop] {
            let mut bb = board.pieces[color.idx()][pt.idx()];
            while bb != 0 {
                let sq = pop_lsb(&mut bb);
                let r = rank_of(sq) as usize;
                let f = file_of(sq) as usize;
                // Base development: bonus for leaving the starting rank.
                let on_back = match color {
                    Color::White => r == 0, // White starts at rank 1 (r=0)
                    Color::Black => r == 7, // Black starts at rank 8 (r=7)
                };
                let mut u = 0;
                if !on_back {
                    u += 6; // developed minor piece (tuned down from 8)
                }
                // Central control bonus.
                if (3..=4).contains(&f) && (3..=4).contains(&r) {
                    u += 4; // central square (tuned down from 5)
                }
                match color {
                    Color::White => dev += u,
                    Color::Black => dev -= u,
                }
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

/// Chebyshev (king-move) distance between two squares.
fn chebyshev(a: u8, b: u8) -> i32 {
    (file_of(a) as i8 - file_of(b) as i8)
        .abs()
        .max((rank_of(a) as i8 - rank_of(b) as i8).abs()) as i32
}

/// Extra endgame weight per square of king-distance difference on a passer.
const PASSER_KING_W: i32 = 6;

/// Pawn-structure term from the pawn owner's perspective. Pawn locations are
/// passed as per-file rank bitmasks (`1 << rank`), one entry per file.
/// Passed pawns also score king distance to the promotion square (own king
/// close / enemy king far is good), scaled up in the endgame via `phase`.
fn pawn_structure(
    color: Color,
    r: usize,
    f: usize,
    own: &[u8; 8],
    enemy: &[u8; 8],
    own_king: Option<u8>,
    enemy_king: Option<u8>,
    phase: i32,
) -> i32 {
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
        // King distance to the passer: our king near the promotion square
        // and their king far away is good (the "square" rule, quantified).
        // Endgame-weighted: full strength with bare kings, gone at phase 24.
        if let (Some(ok), Some(ek)) = (own_king, enemy_king) {
            let promo = match color {
                Color::White => make_sq(f as u8, 7),
                Color::Black => make_sq(f as u8, 0),
            };
            s += (chebyshev(ek, promo) - chebyshev(ok, promo)) * PASSER_KING_W * (24 - phase) / 24;
        }
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
/// late), pawn structure (passed / isolated / doubled, passers scored by king
/// distance to promotion), rooks on open files
/// and on the 7th rank, piece mobility (N/B/R/Q pseudo-legal move counts),
/// king safety (pawn-shield presence, faded out in the endgame),
/// plus a small bishop-pair bonus. All terms are symmetric, so the startpos
/// still evaluates to exactly 0.
pub fn evaluate(board: &Board) -> i32 {
    // First pass: game phase + pawn locations per file (rank bitmasks) +
    // king squares (tracked here so the shield term needs no extra scans).
    let mut phase = 0i32;
    let mut white_pawns = [0u8; 8];
    let mut black_pawns = [0u8; 8];
    let mut white_king = None;
    let mut black_king = None;
    for color in [Color::White, Color::Black] {
        for pt in PIECE_TYPES {
            let mut bb = board.pieces[color.idx()][pt.idx()];
            while bb != 0 {
                let sq = pop_lsb(&mut bb);
                phase += phase_weight(pt);
                if pt == PieceType::Pawn {
                    let f = file_of(sq) as usize;
                    let r = rank_of(sq) as usize;
                    match color {
                        Color::White => white_pawns[f] |= 1 << r,
                        Color::Black => black_pawns[f] |= 1 << r,
                    }
                } else if pt == PieceType::King {
                    match color {
                        Color::White => white_king = Some(sq),
                        Color::Black => black_king = Some(sq),
                    }
                }
            }
        }
    }
    let phase = phase.min(24);

    let mut score = 0i32;
    let mut white_bishops = 0;
    let mut black_bishops = 0;
    for color in [Color::White, Color::Black] {
        for pt in PIECE_TYPES {
            let mut bb = board.pieces[color.idx()][pt.idx()];
            while bb != 0 {
                let sq = pop_lsb(&mut bb);
                // Kings use the tapered table.
                if pt == PieceType::King {
                    let u = king_pst(sq, color, phase);
                    match color {
                        Color::White => score += u,
                        Color::Black => score -= u,
                    }
                    continue;
                }
                let r = rank_of(sq) as usize;
                let f = file_of(sq) as usize;
                // Bishop and knight use tapered tables; others use fixed PST.
                let mut u = if pt == PieceType::Bishop {
                    pt.value() + bishop_pst(sq, color, phase)
                } else if pt == PieceType::Knight {
                    pt.value() + knight_pst(sq, color, phase)
                } else {
                    pt.value() + pst_own(pt, sq, color)
                };
                match pt {
                    PieceType::Pawn => {
                        let (own, enemy, ok, ek) = match color {
                            Color::White => (&white_pawns, &black_pawns, white_king, black_king),
                            Color::Black => (&black_pawns, &white_pawns, black_king, white_king),
                        };
                        u += pawn_structure(color, r, f, own, enemy, ok, ek, phase);
                    }
                    PieceType::Rook => {
                        let (own, enemy) = match color {
                            Color::White => (&white_pawns, &black_pawns),
                            Color::Black => (&black_pawns, &white_pawns),
                        };
                        u += rook_bonus(color, r, f, own, enemy);
                    }
                    PieceType::Bishop => match color {
                        Color::White => white_bishops += 1,
                        Color::Black => black_bishops += 1,
                    },
                    _ => {}
                }
                match pt {
                    PieceType::Knight | PieceType::Bishop | PieceType::Rook | PieceType::Queen => {
                        u += mobility_weight(pt) * mobility_count(board, sq, color, pt) as i32;
                    }
                    _ => {}
                }
                match color {
                    Color::White => score += u,
                    Color::Black => score -= u,
                }
            }
        }
    }
    if white_bishops >= 2 {
        score += 30;
    }
    if black_bishops >= 2 {
        score -= 30;
    }
    // King safety: pawn-shield difference, full weight in the middlegame,
    // fading out as pieces come off (phase scaling like the king PST).
    let w_shield = white_king.map_or(0, |k| king_shield_sq(board, k, Color::White));
    let b_shield = black_king.map_or(0, |k| king_shield_sq(board, k, Color::Black));
    score += (w_shield - b_shield) * phase / 24;
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

    #[test]
    fn eval_is_color_symmetric() {
        // Vertical mirror with colors flipped must negate exactly: every
        // term (material, PSTs, development) is owner-perspective signed.
        // (Catches the old development_bonus bug, which credited Black's
        // development to White; startpos == 0 masked it.)
        let w = Board::from_fen("4k3/8/8/8/3B4/8/8/4K3 w - - 0 1").unwrap();
        let b = Board::from_fen("4k3/8/8/3b4/8/8/8/4K3 w - - 0 1").unwrap();
        assert_eq!(evaluate(&w), -evaluate(&b));
        // White Bd4, phase 1: 330 + bishop PST 19 + development 6 + 4
        // + mobility 13x2.
        assert_eq!(evaluate(&w), 385);
    }

    #[test]
    fn pawn_shield_scores() {
        // Startpos: both kings fully shielded (3 files x 8).
        let b = Board::new_startpos();
        assert_eq!(king_shield(&b, Color::White), 24);
        assert_eq!(king_shield(&b, Color::Black), 24);
        // Kg1 + f2/g2/h2: full shield; h-pawn traded: 16.
        let full = Board::from_fen("7k/8/8/8/8/8/5PPP/6K1 w - - 0 1").unwrap();
        assert_eq!(king_shield(&full, Color::White), 24);
        let traded = Board::from_fen("7k/8/8/8/8/8/5PP1/6K1 w - - 0 1").unwrap();
        assert_eq!(king_shield(&traded, Color::White), 16);
        // Pushed f-pawn (f3, f2 empty) still half-covers: 4 + 8 + 8.
        let pushed = Board::from_fen("7k/8/8/8/8/5P2/6PP/6K1 w - - 0 1").unwrap();
        assert_eq!(king_shield(&pushed, Color::White), 20);
        // Centralised king: no shield term; missing king: 0.
        let mid = Board::from_fen("4k3/8/8/8/4K3/8/8/8 w - - 0 1").unwrap();
        assert_eq!(king_shield(&mid, Color::White), 0);
        // Black mirror: Kg8 + f7/g7/h7.
        let b = Board::from_fen("6k1/5ppp/8/8/8/8/8/6K1 w - - 0 1").unwrap();
        assert_eq!(king_shield(&b, Color::Black), 24);
    }

    #[test]
    fn shield_intact_beats_shield_traded() {
        // Symmetric castled middlegame (R + K + 3 shield pawns each): 0.
        let even = Board::from_fen("r5k1/5ppp/8/8/8/8/5PPP/R5K1 w - - 0 1").unwrap();
        assert_eq!(evaluate(&even), 0);
        // White's h-pawn traded away: material + shield favor Black.
        let cracked = Board::from_fen("r5k1/5ppp/8/8/8/8/5PP1/R5K1 w - - 0 1").unwrap();
        assert!(evaluate(&cracked) < -50);
    }

    #[test]
    fn passer_king_distance_scores() {
        // White Pe5 on a lone file (isolated -12), Ke2 vs Ke7 (phase 0):
        // -12 + passed 35 + distance (1 - 6) x 6 x 24/24 = -30  =>  -7.
        let own = [0u8, 0, 0, 0, 1 << 4, 0, 0, 0];
        let enemy = [0u8; 8];
        assert_eq!(
            pawn_structure(Color::White, 4, 4, &own, &enemy, Some(12), Some(52), 0),
            -7
        );
        // Defender on a6 instead (distance 4): (4 - 6) x 6 = -12  =>  11.
        assert_eq!(
            pawn_structure(Color::White, 4, 4, &own, &enemy, Some(12), Some(40), 0),
            11
        );
        // Full middlegame: distance term fades to 0  =>  -12 + 35 = 23.
        assert_eq!(
            pawn_structure(Color::White, 4, 4, &own, &enemy, Some(12), Some(52), 24),
            23
        );
        // Missing king: term skipped  =>  23.
        assert_eq!(
            pawn_structure(Color::White, 4, 4, &own, &enemy, None, Some(52), 0),
            23
        );
        // Blocked pawn: no passer bonus and no distance term (still
        // isolated: -12).
        let enemy_pawn = [0u8, 0, 0, 0, 1 << 5, 0, 0, 0];
        assert_eq!(
            pawn_structure(Color::White, 4, 4, &own, &enemy_pawn, Some(12), Some(52), 0),
            -12
        );
    }

    #[test]
    fn defender_in_square_scores_lower() {
        // Pe5 + Ke2: defender holding (Ke6, inside the square) evaluates
        // closer to a draw than the defender outside it (Ka6).
        let inside = Board::from_fen("8/8/4k3/4P3/8/8/4K3/8 w - - 0 1").unwrap();
        let outside = Board::from_fen("8/8/k7/4P3/8/8/4K3/8 w - - 0 1").unwrap();
        assert!(evaluate(&outside) > evaluate(&inside));
    }

    #[test]
    fn mobility_term_scores() {
        // Ke1 + Na1 vs Ke8, phase 1: material 320, knight PST -50
        // (corner in both MG and EG tables), mobility 2 moves x 2 = 4,
        // kings cancel (both tapered to -28), knight undeveloped (no dev
        // bonus). Total: 320 - 50 + 4 = 274 (270 without the term).
        let b = Board::from_fen("4k3/8/8/8/8/8/8/N3K3 w - - 0 1").unwrap();
        assert_eq!(evaluate(&b), 274);
    }
}
