//! Bitboard utilities: square-indexed attack sets over occupancy bitboards.
//! Square indexing matches `types.rs` (0 = a1, ..., 63 = h8); bit `i` of a
//! `u64` is square `i`. Classical ray-based sliding attacks (no magics):
//! leapers/pawns are single table/shift ops, sliders walk rays over the
//! occupancy bits.

use std::sync::OnceLock;

use crate::types::{file_of, rank_of, Color};

/// Bit with only `sq` set.
#[inline]
pub fn bit(sq: u8) -> u64 {
    1u64 << sq
}

/// Remove and return the index of the least-significant set bit.
/// Caller must guarantee `*bb != 0`.
#[inline]
pub fn pop_lsb(bb: &mut u64) -> u8 {
    let sq = bb.trailing_zeros() as u8;
    *bb &= *bb - 1;
    sq
}

// ---------------------------------------------------------------------------
// Leaper tables (precomputed once, `OnceLock` like the Zobrist tables)
// ---------------------------------------------------------------------------

fn build_leaper(deltas: &[(i8, i8)]) -> [u64; 64] {
    let mut t = [0u64; 64];
    for sq in 0..64u8 {
        let f = file_of(sq) as i8;
        let r = rank_of(sq) as i8;
        let mut bb = 0u64;
        for (df, dr) in deltas {
            let nf = f + df;
            let nr = r + dr;
            if (0..8).contains(&nf) && (0..8).contains(&nr) {
                bb |= bit((nr * 8 + nf) as u8);
            }
        }
        t[sq as usize] = bb;
    }
    t
}

const KNIGHT_DELTAS: [(i8, i8); 8] = [
    (1, 2),
    (2, 1),
    (2, -1),
    (1, -2),
    (-1, -2),
    (-2, -1),
    (-2, 1),
    (-1, 2),
];

const KING_DELTAS: [(i8, i8); 8] = [
    (1, 1),
    (1, 0),
    (1, -1),
    (0, 1),
    (0, -1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

fn knight_table() -> &'static [u64; 64] {
    static T: OnceLock<[u64; 64]> = OnceLock::new();
    T.get_or_init(|| build_leaper(&KNIGHT_DELTAS))
}

fn king_table() -> &'static [u64; 64] {
    static T: OnceLock<[u64; 64]> = OnceLock::new();
    T.get_or_init(|| build_leaper(&KING_DELTAS))
}

/// Squares a knight on `sq` attacks (board edges handled by the table).
#[inline]
pub fn knight_attacks(sq: u8) -> u64 {
    knight_table()[sq as usize]
}

/// Squares a king on `sq` attacks.
#[inline]
pub fn king_attacks(sq: u8) -> u64 {
    king_table()[sq as usize]
}

// ---------------------------------------------------------------------------
// Pawn attacks
// ---------------------------------------------------------------------------

/// Squares a pawn of color `by` would stand on to attack `sq`
/// (i.e. the reverse of `pawn_attacks`; used for attack detection).
pub fn pawn_attackers(sq: u8, by: Color) -> u64 {
    let f = file_of(sq);
    let r = rank_of(sq);
    let mut bb = 0u64;
    match by {
        Color::White => {
            if r > 0 {
                if f < 7 {
                    bb |= bit(sq - 7);
                }
                if f > 0 {
                    bb |= bit(sq - 9);
                }
            }
        }
        Color::Black => {
            if r < 7 {
                if f > 0 {
                    bb |= bit(sq + 7);
                }
                if f < 7 {
                    bb |= bit(sq + 9);
                }
            }
        }
    }
    bb
}

// ---------------------------------------------------------------------------
// Sliding attacks (classical ray walk over occupancy bits)
// ---------------------------------------------------------------------------

fn ray_attacks(sq: u8, occ: u64, deltas: &[(i8, i8)]) -> u64 {
    let f = file_of(sq) as i8;
    let r = rank_of(sq) as i8;
    let mut bb = 0u64;
    for (df, dr) in deltas {
        let mut nf = f + df;
        let mut nr = r + dr;
        while (0..8).contains(&nf) && (0..8).contains(&nr) {
            let to = (nr * 8 + nf) as u8;
            bb |= bit(to);
            if occ & bit(to) != 0 {
                break; // blocked (capture square included)
            }
            nf += df;
            nr += dr;
        }
    }
    bb
}

const DIAG: [(i8, i8); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];
const ORTH: [(i8, i8); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

/// Squares a bishop on `sq` attacks given occupancy `occ`. The first blocker
/// of either color is included; move generation masks with `!own_occupancy`.
#[inline]
pub fn bishop_attacks(sq: u8, occ: u64) -> u64 {
    ray_attacks(sq, occ, &DIAG)
}

/// Squares a rook on `sq` attacks given occupancy `occ` (first blocker of
/// either color included; mask with `!own_occupancy` for move targets).
#[inline]
pub fn rook_attacks(sq: u8, occ: u64) -> u64 {
    ray_attacks(sq, occ, &ORTH)
}

/// Squares a queen on `sq` attacks given occupancy `occ` (same contract).
#[inline]
pub fn queen_attacks(sq: u8, occ: u64) -> u64 {
    ray_attacks(sq, occ, &DIAG) | ray_attacks(sq, occ, &ORTH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knight_table_spots() {
        // a1 attacks b3 + c2 only.
        assert_eq!(knight_attacks(0), bit(17) | bit(10));
        // d4 (27) attacks 8 squares.
        assert_eq!(knight_attacks(27).count_ones(), 8);
        // h8 (63) attacks f7 + g6 only.
        assert_eq!(knight_attacks(63), bit(53) | bit(46));
    }

    #[test]
    fn king_table_spots() {
        // e1 (4): d1, f1, d2, e2, f2.
        assert_eq!(
            king_attacks(4),
            bit(3) | bit(5) | bit(11) | bit(12) | bit(13)
        );
        assert_eq!(king_attacks(27).count_ones(), 8);
    }

    #[test]
    fn slider_empty_board() {
        // Rook d4 (27): whole rank + file minus self = 14.
        assert_eq!(rook_attacks(27, 0).count_ones(), 14);
        // Bishop d4: 4 + 3 + 3 + 3 = 13.
        assert_eq!(bishop_attacks(27, 0).count_ones(), 13);
        assert_eq!(queen_attacks(27, 0).count_ones(), 27);
    }

    #[test]
    fn slider_blockers() {
        // Rook d4, own pawn e4, enemy pawn d6. Raw set includes the first
        // blocker of either color: a4,b4,c4,e4 + d5,d6 + d3,d2,d1 = 9.
        let occ = bit(28) | bit(43);
        let a = rook_attacks(27, occ);
        assert_eq!(a.count_ones(), 9);
        assert!(a & bit(43) != 0); // enemy blocker (capture) included
        assert!(a & bit(28) != 0); // own blocker included in the raw set
                                   // Masked with !own: 8 legal targets, nothing beyond the blockers.
        let legal = a & !bit(28);
        assert_eq!(legal.count_ones(), 8);
        assert!(legal & bit(29) == 0); // beyond own blocker excluded
        assert!(legal & bit(51) == 0); // beyond enemy blocker excluded
                                       // Bishop d4 walled by blockers on c5/e5/c3/e3: 4 (one per ray).
        let occ = bit(34) | bit(36) | bit(18) | bit(20);
        assert_eq!(bishop_attacks(27, occ).count_ones(), 4);
    }

    #[test]
    fn pawn_attack_edges() {
        use crate::types::sq_from_name;
        // Attackers of d4: white from c3/e3, black from c5/e5.
        assert_eq!(
            pawn_attackers(sq_from_name("d4").unwrap(), Color::White),
            bit(18) | bit(20)
        );
        assert_eq!(
            pawn_attackers(sq_from_name("d4").unwrap(), Color::Black),
            bit(34) | bit(36)
        );
        // Nobody attacks from off the board: white attackers of a1: none.
        assert_eq!(pawn_attackers(0, Color::White), 0);
    }

    #[test]
    fn pop_lsb_order() {
        let mut bb = bit(3) | bit(5) | bit(60);
        assert_eq!(pop_lsb(&mut bb), 3);
        assert_eq!(pop_lsb(&mut bb), 5);
        assert_eq!(pop_lsb(&mut bb), 60);
        assert_eq!(bb, 0);
    }
}
