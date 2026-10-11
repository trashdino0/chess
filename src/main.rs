mod bitboard;
mod board;
mod dino;
mod eval;
mod movegen;
mod search;
mod types;
mod uci;

fn main() {
    // CLI shortcuts (bypass UCI loop):
    //   dinochess perft <depth> [fen]  - run a perft count
    //   dinochess bench [depth]        - run a small search benchmark
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        match args[1].as_str() {
            "perft" => {
                let depth: u32 = args.get(2).and_then(|d| d.parse().ok()).unwrap_or(4);
                let fen = args
                    .get(3)
                    .map(|s| s.as_str())
                    .unwrap_or(board::STARTPOS_FEN);
                let mut b = board::Board::from_fen(fen).expect("bad FEN");
                let t = std::time::Instant::now();
                let n = movegen::perft(&mut b, depth);
                println!("perft({depth}) = {n} in {:?}", t.elapsed());
                return;
            }
            "bench" => {
                let depth: u8 = args.get(2).and_then(|d| d.parse().ok()).unwrap_or(5);
                uci::bench_cli(depth);
                return;
            }
            "search" => {
                // dinochess search <depth> [fen] — direct search, no UCI loop.
                let depth: u8 = args.get(2).and_then(|d| d.parse().ok()).unwrap_or(5);
                let fen = args
                    .get(3)
                    .map(|s| s.as_str())
                    .unwrap_or(board::STARTPOS_FEN);
                uci::search_cli(depth, fen);
                return;
            }
            _ => {}
        }
    }
    uci::run();
}
