//! Pseudo-legal + legal move generation over bitboards, perft.
//!
//! Generation order is type-major (pawns, knights, king, bishops, rooks,
//! queens; targets ascending) rather than the old square-major order. Perft
//! counts are order-independent; the stable move-ordering ties may break
//! differently (reported per sub-step, judged by the self-play gate).

use crate::bitboard::{
    bishop_attacks, bit, king_attacks, knight_attacks, pop_lsb, queen_attacks, rook_attacks,
};
use crate::board::Board;
use crate::types::*;

pub fn generate_pseudo(board: &Board) -> Vec<Move> {
    let mut moves = Vec::with_capacity(64);
    let side = board.side;
    let si = side.idx();
    let free = !board.occ[si]; // empty or enemy
                               // Pawns (scalar pushes/captures, ascending).
    let mut bb = board.pieces[si][PieceType::Pawn.idx()];
    while bb != 0 {
        gen_pawn(board, pop_lsb(&mut bb), side, &mut moves);
    }
    // Knights.
    let mut bb = board.pieces[si][PieceType::Knight.idx()];
    while bb != 0 {
        let sq = pop_lsb(&mut bb);
        push_set(&mut moves, sq, knight_attacks(sq) & free);
    }
    // King (+ castling).
    let mut bb = board.pieces[si][PieceType::King.idx()];
    while bb != 0 {
        let sq = pop_lsb(&mut bb);
        push_set(&mut moves, sq, king_attacks(sq) & free);
    }
    gen_castling(board, side, &mut moves);
    // Sliders.
    let occ = board.occ_all;
    let mut bb = board.pieces[si][PieceType::Bishop.idx()];
    while bb != 0 {
        let sq = pop_lsb(&mut bb);
        push_set(&mut moves, sq, bishop_attacks(sq, occ) & free);
    }
    let mut bb = board.pieces[si][PieceType::Rook.idx()];
    while bb != 0 {
        let sq = pop_lsb(&mut bb);
        push_set(&mut moves, sq, rook_attacks(sq, occ) & free);
    }
    let mut bb = board.pieces[si][PieceType::Queen.idx()];
    while bb != 0 {
        let sq = pop_lsb(&mut bb);
        push_set(&mut moves, sq, queen_attacks(sq, occ) & free);
    }
    moves
}

/// Push one quiet move per set bit (targets ascending).
fn push_set(moves: &mut Vec<Move>, from: u8, mut targets: u64) {
    while targets != 0 {
        moves.push(Move::new(from, pop_lsb(&mut targets), None));
    }
}

fn push_pawn_move(moves: &mut Vec<Move>, from: u8, to: u8, promo_rank: bool) {
    if promo_rank {
        for pt in [
            PieceType::Queen,
            PieceType::Rook,
            PieceType::Bishop,
            PieceType::Knight,
        ] {
            moves.push(Move::new(from, to, Some(pt)));
        }
    } else {
        moves.push(Move::new(from, to, None));
    }
}

fn gen_pawn(board: &Board, sq: u8, side: Color, moves: &mut Vec<Move>) {
    let f = file_of(sq) as i8;
    let r = rank_of(sq) as i8;
    let (dir, start_rank, promo_rank_next) = match side {
        Color::White => (1i8, 1i8, 7i8),
        Color::Black => (-1i8, 6i8, 0i8),
    };
    let ei = side.opposite().idx();
    let one_r = r + dir;
    // Single + double push.
    if one_r >= 0 && one_r < 8 {
        let one = make_sq(f as u8, one_r as u8);
        if board.occ_all & bit(one) == 0 {
            push_pawn_move(moves, sq, one, one_r == promo_rank_next);
            if r == start_rank {
                let two_r = r + 2 * dir;
                let two = make_sq(f as u8, two_r as u8);
                if board.occ_all & bit(two) == 0 {
                    moves.push(Move::new(sq, two, None));
                }
            }
        }
    }
    // Captures (incl. en passant).
    for df in [-1i8, 1i8] {
        let nf = f + df;
        let nr = r + dir;
        if nf < 0 || nf >= 8 || nr < 0 || nr >= 8 {
            continue;
        }
        let to = make_sq(nf as u8, nr as u8);
        if board.occ[ei] & bit(to) != 0 {
            push_pawn_move(moves, sq, to, nr == promo_rank_next);
        } else if Some(to) == board.ep {
            // EP capture is never a promotion (5th rank).
            moves.push(Move::new(sq, to, None));
        }
    }
}

/// Pseudo-legal captures/promotions only (for quiescence): captures,
/// en-passant captures, and all promotions (including quiet promotion
/// pushes). Quiet non-promotions and castling are never emitted.
pub fn generate_pseudo_captures(board: &Board) -> Vec<Move> {
    let mut moves = Vec::with_capacity(16);
    let side = board.side;
    let si = side.idx();
    let enemy = board.occ[si ^ 1];
    let mut bb = board.pieces[si][PieceType::Pawn.idx()];
    while bb != 0 {
        gen_pawn_captures(board, pop_lsb(&mut bb), side, &mut moves);
    }
    let mut bb = board.pieces[si][PieceType::Knight.idx()];
    while bb != 0 {
        let sq = pop_lsb(&mut bb);
        push_set(&mut moves, sq, knight_attacks(sq) & enemy);
    }
    let mut bb = board.pieces[si][PieceType::King.idx()];
    while bb != 0 {
        let sq = pop_lsb(&mut bb);
        push_set(&mut moves, sq, king_attacks(sq) & enemy);
    }
    let occ = board.occ_all;
    let mut bb = board.pieces[si][PieceType::Bishop.idx()];
    while bb != 0 {
        let sq = pop_lsb(&mut bb);
        push_set(&mut moves, sq, bishop_attacks(sq, occ) & enemy);
    }
    let mut bb = board.pieces[si][PieceType::Rook.idx()];
    while bb != 0 {
        let sq = pop_lsb(&mut bb);
        push_set(&mut moves, sq, rook_attacks(sq, occ) & enemy);
    }
    let mut bb = board.pieces[si][PieceType::Queen.idx()];
    while bb != 0 {
        let sq = pop_lsb(&mut bb);
        push_set(&mut moves, sq, queen_attacks(sq, occ) & enemy);
    }
    moves
}

fn gen_pawn_captures(board: &Board, sq: u8, side: Color, moves: &mut Vec<Move>) {
    let f = file_of(sq) as i8;
    let r = rank_of(sq) as i8;
    let (dir, promo_rank_next) = match side {
        Color::White => (1i8, 7i8),
        Color::Black => (-1i8, 0i8),
    };
    let ei = side.opposite().idx();
    let one_r = r + dir;
    // Quiet promotion push (no capture) is still a "capture" for quiescence.
    if one_r >= 0 && one_r < 8 {
        let one = make_sq(f as u8, one_r as u8);
        if board.occ_all & bit(one) == 0 && one_r == promo_rank_next {
            push_pawn_move(moves, sq, one, true);
        }
    }
    // Captures (incl. en passant); promotion handled per target rank.
    for df in [-1i8, 1i8] {
        let nf = f + df;
        let nr = r + dir;
        if nf < 0 || nf >= 8 || nr < 0 || nr >= 8 {
            continue;
        }
        let to = make_sq(nf as u8, nr as u8);
        if board.occ[ei] & bit(to) != 0 {
            push_pawn_move(moves, sq, to, nr == promo_rank_next);
        } else if Some(to) == board.ep {
            moves.push(Move::new(sq, to, None));
        }
    }
}

/// Pseudo-legal destination count for one piece, for eval mobility.
/// Counts quiet + capture destinations via attack-set popcounts (not
/// legality-checked). Pawns, kings and empty squares count 0.
pub fn mobility_count(board: &Board, sq: u8) -> u32 {
    let (side, pt) = match board.squares[sq as usize] {
        Some(p) => (p.color, p.pt),
        None => return 0,
    };
    let free = !board.occ[side.idx()];
    match pt {
        PieceType::Knight => (knight_attacks(sq) & free).count_ones(),
        PieceType::Bishop => (bishop_attacks(sq, board.occ_all) & free).count_ones(),
        PieceType::Rook => (rook_attacks(sq, board.occ_all) & free).count_ones(),
        PieceType::Queen => (queen_attacks(sq, board.occ_all) & free).count_ones(),
        _ => 0,
    }
}

fn gen_castling(board: &Board, side: Color, moves: &mut Vec<Move>) {
    let si = side.idx();
    let kings = board.pieces[si][PieceType::King.idx()];
    let rooks = board.pieces[si][PieceType::Rook.idx()];
    let occ = board.occ_all;
    let empty = |sq: u8| occ & bit(sq) == 0;
    match side {
        Color::White => {
            if kings & bit(E1) == 0 {
                return;
            }
            // Kingside: f1, g1 empty; e1, f1, g1 not attacked.
            if board.castling & WK != 0 && empty(5) && empty(G1) && rooks & bit(H1) != 0 {
                let b = Color::Black;
                if !board.is_square_attacked(E1, b)
                    && !board.is_square_attacked(5, b)
                    && !board.is_square_attacked(G1, b)
                {
                    moves.push(Move::new(E1, G1, None));
                }
            }
            // Queenside: b1, c1, d1 empty; e1, d1, c1 not attacked.
            if board.castling & WQ != 0 && empty(3) && empty(C1) && empty(1) && rooks & bit(A1) != 0
            {
                let b = Color::Black;
                if !board.is_square_attacked(E1, b)
                    && !board.is_square_attacked(3, b)
                    && !board.is_square_attacked(C1, b)
                {
                    moves.push(Move::new(E1, C1, None));
                }
            }
        }
        Color::Black => {
            if kings & bit(E8) == 0 {
                return;
            }
            if board.castling & BK != 0 && empty(61) && empty(G8) && rooks & bit(H8) != 0 {
                let w = Color::White;
                if !board.is_square_attacked(E8, w)
                    && !board.is_square_attacked(61, w)
                    && !board.is_square_attacked(G8, w)
                {
                    moves.push(Move::new(E8, G8, None));
                }
            }
            if board.castling & BQ != 0
                && empty(59)
                && empty(C8)
                && empty(57)
                && rooks & bit(A8) != 0
            {
                let w = Color::White;
                if !board.is_square_attacked(E8, w)
                    && !board.is_square_attacked(59, w)
                    && !board.is_square_attacked(C8, w)
                {
                    moves.push(Move::new(E8, C8, None));
                }
            }
        }
    }
}

/// All legal moves (pseudo-legal filtered by king safety).
pub fn generate_legal(board: &mut Board) -> Vec<Move> {
    let pseudos = generate_pseudo(board);
    let side = board.side;
    let mut legal = Vec::with_capacity(pseudos.len());
    for mv in pseudos {
        let undo = board.make_move(mv);
        // After the move, our king must not be in check.
        let ok = !board.in_check(side);
        board.unmake_move(undo);
        if ok {
            legal.push(mv);
        }
    }
    legal
}

/// Legal captures (and promotions) for quiescence search.
/// Generates only capture/EP/promotion pseudos, then legality-checks just
/// that smaller set (same make/unmake + in_check filter as `generate_legal`).
pub fn generate_captures(board: &mut Board) -> Vec<Move> {
    let pseudos = generate_pseudo_captures(board);
    let side = board.side;
    let mut legal = Vec::with_capacity(pseudos.len());
    for mv in pseudos {
        let undo = board.make_move(mv);
        let ok = !board.in_check(side);
        board.unmake_move(undo);
        if ok {
            legal.push(mv);
        }
    }
    legal
}

/// Move count test. `board` is restored afterwards.
pub fn perft(board: &mut Board, depth: u32) -> u64 {
    if depth == 0 {
        return 1;
    }
    let moves = generate_legal(board);
    if depth == 1 {
        return moves.len() as u64;
    }
    let mut nodes = 0u64;
    for mv in moves {
        let undo = board.make_move(mv);
        nodes += perft(board, depth - 1);
        board.unmake_move(undo);
    }
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perft_fen(fen: &str, depth: u32) -> u64 {
        let mut b = Board::from_fen(fen).unwrap();
        perft(&mut b, depth)
    }

    #[test]
    fn startpos_perft() {
        assert_eq!(perft_fen(crate::board::STARTPOS_FEN, 1), 20);
        assert_eq!(perft_fen(crate::board::STARTPOS_FEN, 2), 400);
        assert_eq!(perft_fen(crate::board::STARTPOS_FEN, 3), 8_902);
    }

    #[test]
    fn startpos_perft_4() {
        assert_eq!(perft_fen(crate::board::STARTPOS_FEN, 4), 197_281);
    }

    #[test]
    fn kiwipete_perft() {
        // Published numbers: https://chessprogramming.org/Perft_Results
        let fen = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
        assert_eq!(perft_fen(fen, 1), 48);
        assert_eq!(perft_fen(fen, 2), 2_039);
        assert_eq!(perft_fen(fen, 3), 97_862);
    }

    #[test]
    fn kiwipete_perft_4() {
        // Published d4 = 4_085_603 (regression gate for movegen changes).
        let fen = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
        assert_eq!(perft_fen(fen, 4), 4_085_603);
    }

    #[test]
    fn endgame_perft() {
        // Position 3 (EP/castling stress + promotion-heavy).
        // Published: https://chessprogramming.org/Perft_Results
        let fen = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";
        assert_eq!(perft_fen(fen, 1), 14);
        assert_eq!(perft_fen(fen, 2), 191);
        assert_eq!(perft_fen(fen, 3), 2_812);
    }

    #[test]
    fn position3_perft_deep() {
        // Published d4 = 43_238, d5 = 674_624.
        let fen = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";
        assert_eq!(perft_fen(fen, 4), 43_238);
        assert_eq!(perft_fen(fen, 5), 674_624);
    }

    #[test]
    fn mobility_counts() {
        // Knight d4: 8; knight a1: b3 + c2 = 2.
        let b = Board::from_fen("4k3/8/8/8/3N4/8/8/4K3 w - - 0 1").unwrap();
        assert_eq!(mobility_count(&b, 27), 8);
        let b = Board::from_fen("4k3/8/8/8/8/8/8/N3K3 w - - 0 1").unwrap();
        assert_eq!(mobility_count(&b, 0), 2);
        // Bishop d4 on an open board: 13; fully blocked by own pawns: 0.
        let b = Board::from_fen("4k3/8/8/8/3B4/8/8/4K3 w - - 0 1").unwrap();
        assert_eq!(mobility_count(&b, 27), 13);
        let b = Board::from_fen("4k3/8/8/2P1P3/3B4/2P1P3/8/4K3 w - - 0 1").unwrap();
        assert_eq!(mobility_count(&b, 27), 0);
        // Rook d4: 14; queen d4: 13 + 14 = 27.
        let b = Board::from_fen("4k3/8/8/8/3R4/8/8/4K3 w - - 0 1").unwrap();
        assert_eq!(mobility_count(&b, 27), 14);
        let b = Board::from_fen("4k3/8/8/8/3Q4/8/8/4K3 w - - 0 1").unwrap();
        assert_eq!(mobility_count(&b, 27), 27);
        // Pawns, kings and empty squares count 0.
        let b = Board::new_startpos();
        assert_eq!(mobility_count(&b, 8), 0); // white a-pawn
        assert_eq!(mobility_count(&b, 4), 0); // white king
        assert_eq!(mobility_count(&b, 27), 0); // empty d4
    }

    fn sort_moves(moves: &mut [Move]) {
        moves.sort_by_key(|m| {
            (
                m.from,
                m.to,
                m.promotion.map(|p| p.idx() as u8).unwrap_or(255),
            )
        });
    }

    #[test]
    fn captures_match_legal_filter() {
        // Direct pseudo-capture generation must agree with the old
        // filter-based definition (legal moves that capture, EP, or promote).
        let fens = [
            crate::board::STARTPOS_FEN,
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
            "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R b KQkq - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
            "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 b - - 0 1",
            "r1bqkbnr/pppp1ppp/2n5/4p2Q/2B1P3/8/PPPP1PPP/RNB1K1NR w KQkq - 1 1",
            "rnbqkbnr/pp1ppppp/8/2p5/4P3/5N2/PPPP1PPP/RNBQKB1R b KQkq - 1 2",
            "r1bqkb1r/pppp1ppp/2n2n2/1B2p3/4P3/5N2/PPPP1PPP/RNBQK2R b KQkq - 3 3",
            "2kr3r/ppp1qppp/2n1pn2/3p4/3P1B2/2P1PN2/PPQ2PPP/R3K2R w KQ - 0 1",
            "5rk1/pp3ppp/2p2n2/3p4/3P4/2P1PN2/PP3PPP/5RK1 w - - 0 1",
            "8/5pk1/5p1p/8/8/1P3P1P/5PK1/8 w - - 0 1",
            "6k1/5ppp/8/8/8/8/5PPP/6K1 w - - 0 1",
            // En passant available.
            "rnbqkbnr/ppp1pppp/8/3pP3/8/8/PPPP1PPP/RNBQKBNR w KQkq d6 0 3",
            "rnbqkbnr/pppp1ppp/8/8/3pP3/8/PPPP1PPP/RNBQKBNR b KQkq e3 0 3",
            // Promotions (quiet + captures).
            "8/2P2k2/8/8/8/8/5K2/8 w - - 0 1",
            "3r2k1/2P4p/8/8/8/8/5K2/8 w - - 0 1",
            "8/5k2/8/8/8/8/2p2K2/8 b - - 0 1",
            // In check (quiescence searches evasions; captures still defined).
            "r1bqkbnr/pppp1ppp/2n5/4p2Q/2B1P3/8/PPPP1PPP/RNB1K1NR b KQkq - 1 1",
            "k7/8/8/8/8/8/1q6/K7 w - - 0 1",
            "r3k2r/8/8/8/8/8/8/R3K2R w KQkq - 0 1",
        ];
        for fen in fens {
            let mut b = Board::from_fen(fen).unwrap();
            let mut direct = generate_captures(&mut b);
            let mut filtered: Vec<Move> = generate_legal(&mut b)
                .into_iter()
                .filter(|mv| {
                    mv.promotion.is_some()
                        || b.squares[mv.to as usize].is_some()
                        // EP capture: pawn moving to the EP square (a quiet
                        // non-pawn move landing on the EP square, e.g. Bh6
                        // with EP=h6, is NOT a capture — the old filter
                        // over-included those; the direct generator drops them).
                        || (Some(mv.to) == b.ep
                            && b.squares[mv.from as usize].is_some_and(|p| {
                                p.pt == PieceType::Pawn && p.color == b.side
                            }))
                })
                .collect();
            sort_moves(&mut direct);
            sort_moves(&mut filtered);
            assert_eq!(direct, filtered, "capture mismatch on {fen}");
            // And on every child (one ply deeper), to catch EP-square and
            // promotion edge cases the root set misses.
            for mv in generate_legal(&mut b) {
                let undo = b.make_move(mv);
                let mut direct = generate_captures(&mut b);
                let mut filtered: Vec<Move> = generate_legal(&mut b)
                    .into_iter()
                    .filter(|mv| {
                        mv.promotion.is_some()
                            || b.squares[mv.to as usize].is_some()
                            || (Some(mv.to) == b.ep
                                && b.squares[mv.from as usize]
                                    .is_some_and(|p| p.pt == PieceType::Pawn && p.color == b.side))
                    })
                    .collect();
                sort_moves(&mut direct);
                sort_moves(&mut filtered);
                assert_eq!(
                    direct,
                    filtered,
                    "child capture mismatch on {} after {}",
                    b.to_fen(),
                    mv.to_uci()
                );
                b.unmake_move(undo);
            }
        }
    }
}
