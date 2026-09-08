//! Pseudo-legal + legal move generation, perft.

use crate::board::Board;
use crate::types::*;

pub fn generate_pseudo(board: &Board) -> Vec<Move> {
    let mut moves = Vec::with_capacity(64);
    let side = board.side;
    for sq in 0..64u8 {
        let piece = match board.squares[sq as usize] {
            Some(p) if p.color == side => p,
            _ => continue,
        };
        match piece.pt {
            PieceType::Pawn => gen_pawn(board, sq, side, &mut moves),
            PieceType::Knight => gen_stepper(board, sq, side, &KNIGHT_JUMPS, &mut moves),
            PieceType::King => {
                gen_stepper(board, sq, side, &KING_STEPS, &mut moves);
                gen_castling(board, side, &mut moves);
            }
            PieceType::Bishop => gen_slider(board, sq, side, &DIAG, &mut moves),
            PieceType::Rook => gen_slider(board, sq, side, &ORTH, &mut moves),
            PieceType::Queen => {
                gen_slider(board, sq, side, &DIAG, &mut moves);
                gen_slider(board, sq, side, &ORTH, &mut moves);
            }
        }
    }
    moves
}

const KNIGHT_JUMPS: [(i8, i8); 8] = [
    (1, 2),
    (2, 1),
    (2, -1),
    (1, -2),
    (-1, -2),
    (-2, -1),
    (-2, 1),
    (-1, 2),
];

const KING_STEPS: [(i8, i8); 8] = [
    (1, 1),
    (1, 0),
    (1, -1),
    (0, 1),
    (0, -1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

const DIAG: [(i8, i8); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];
const ORTH: [(i8, i8); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

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
    let one_r = r + dir;
    // Single + double push.
    if one_r >= 0 && one_r < 8 {
        let one = make_sq(f as u8, one_r as u8);
        if board.squares[one as usize].is_none() {
            push_pawn_move(moves, sq, one, one_r == promo_rank_next);
            if r == start_rank {
                let two_r = r + 2 * dir;
                let two = make_sq(f as u8, two_r as u8);
                if board.squares[two as usize].is_none() {
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
        match board.squares[to as usize] {
            Some(p) if p.color != side => {
                push_pawn_move(moves, sq, to, nr == promo_rank_next);
            }
            _ => {
                if Some(to) == board.ep {
                    // EP capture is never a promotion (5th rank).
                    moves.push(Move::new(sq, to, None));
                }
            }
        }
    }
}

fn gen_stepper(
    board: &Board,
    sq: u8,
    side: Color,
    deltas: &[(i8, i8)],
    moves: &mut Vec<Move>,
) {
    let f = file_of(sq) as i8;
    let r = rank_of(sq) as i8;
    for (df, dr) in deltas {
        let nf = f + df;
        let nr = r + dr;
        if nf < 0 || nf >= 8 || nr < 0 || nr >= 8 {
            continue;
        }
        let to = make_sq(nf as u8, nr as u8);
        match board.squares[to as usize] {
            None => moves.push(Move::new(sq, to, None)),
            Some(p) if p.color != side => moves.push(Move::new(sq, to, None)),
            _ => {}
        }
    }
}

fn gen_slider(
    board: &Board,
    sq: u8,
    side: Color,
    deltas: &[(i8, i8)],
    moves: &mut Vec<Move>,
) {
    let f = file_of(sq) as i8;
    let r = rank_of(sq) as i8;
    for (df, dr) in deltas {
        let mut nf = f + df;
        let mut nr = r + dr;
        while nf >= 0 && nf < 8 && nr >= 0 && nr < 8 {
            let to = make_sq(nf as u8, nr as u8);
            match board.squares[to as usize] {
                None => moves.push(Move::new(sq, to, None)),
                Some(p) if p.color != side => {
                    moves.push(Move::new(sq, to, None));
                    break;
                }
                _ => break,
            }
            nf += df;
            nr += dr;
        }
    }
}

fn gen_castling(board: &Board, side: Color, moves: &mut Vec<Move>) {
    match side {
        Color::White => {
            if board.squares[E1 as usize]
                != Some(Piece::new(PieceType::King, Color::White))
            {
                return;
            }
            // Kingside: f1, g1 empty; e1, f1, g1 not attacked.
            if board.castling & WK != 0
                && board.squares[5].is_none()
                && board.squares[G1 as usize].is_none()
                && board.squares[H1 as usize]
                    == Some(Piece::new(PieceType::Rook, Color::White))
            {
                let b = Color::Black;
                if !board.is_square_attacked(E1, b)
                    && !board.is_square_attacked(5, b)
                    && !board.is_square_attacked(G1, b)
                {
                    moves.push(Move::new(E1, G1, None));
                }
            }
            // Queenside: b1, c1, d1 empty; e1, d1, c1 not attacked.
            if board.castling & WQ != 0
                && board.squares[3].is_none()
                && board.squares[C1 as usize].is_none()
                && board.squares[1].is_none()
                && board.squares[A1 as usize]
                    == Some(Piece::new(PieceType::Rook, Color::White))
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
            if board.squares[E8 as usize]
                != Some(Piece::new(PieceType::King, Color::Black))
            {
                return;
            }
            if board.castling & BK != 0
                && board.squares[61].is_none()
                && board.squares[G8 as usize].is_none()
                && board.squares[H8 as usize]
                    == Some(Piece::new(PieceType::Rook, Color::Black))
            {
                let w = Color::White;
                if !board.is_square_attacked(E8, w)
                    && !board.is_square_attacked(61, w)
                    && !board.is_square_attacked(G8, w)
                {
                    moves.push(Move::new(E8, G8, None));
                }
            }
            if board.castling & BQ != 0
                && board.squares[59].is_none()
                && board.squares[C8 as usize].is_none()
                && board.squares[57].is_none()
                && board.squares[A8 as usize]
                    == Some(Piece::new(PieceType::Rook, Color::Black))
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
pub fn generate_captures(board: &mut Board) -> Vec<Move> {
    generate_legal(board)
        .into_iter()
        .filter(|mv| {
            mv.promotion.is_some()
                || board.squares[mv.to as usize].is_some()
                // EP capture: target square empty but it is the EP square.
                || Some(mv.to) == board.ep
        })
        .collect()
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
        let fen = "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1";
        assert_eq!(perft_fen(fen, 1), 48);
        assert_eq!(perft_fen(fen, 2), 2_039);
        assert_eq!(perft_fen(fen, 3), 97_862);
    }

    #[test]
    fn endgame_perft() {
        // Kiwipete-like EP/castling stress + promotion-heavy position.
        let fen = "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1";
        assert_eq!(perft_fen(fen, 1), 14);
        assert_eq!(perft_fen(fen, 2), 191);
        assert_eq!(perft_fen(fen, 3), 2_812);
    }
}
