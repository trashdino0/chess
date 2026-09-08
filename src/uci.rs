//! UCI protocol loop, plus `perft` / `bench` helpers and debug commands.
//!
//! Concurrency model (std only): a dedicated thread reads stdin lines and
//! forwards them over an mpsc channel. The main thread processes commands;
//! `go` runs the search on the main thread while the search periodically
//! polls the channel for `stop`/`quit` via a callback.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use crate::board::{Board, STARTPOS_FEN};
use crate::movegen::{generate_legal, perft};
use crate::search::{Limits, TT, search_root};
use crate::types::Move;

pub const ENGINE_NAME: &str = "Ferrum 1.1";
pub const ENGINE_AUTHOR: &str = "opencode";

pub struct Engine {
    board: Board,
    tt: TT,
    hash_mb: usize,
    quit_after_search: Arc<AtomicBool>,
}

impl Engine {
    pub fn new() -> Engine {
        Engine {
            board: Board::new_startpos(),
            tt: TT::new(16),
            hash_mb: 16,
            quit_after_search: Arc::new(AtomicBool::new(false)),
        }
    }

    fn set_hash(&mut self, mb: usize) {
        self.hash_mb = mb.clamp(1, 1024);
        self.tt = TT::new(self.hash_mb);
    }
}

/// Spawn a thread forwarding stdin lines to `tx`. Returns the join handle.
fn spawn_stdin_reader(tx: mpsc::Sender<String>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
}

pub fn run() {
    let (tx, rx) = mpsc::channel::<String>();
    let _reader = spawn_stdin_reader(tx);

    let mut eng = Engine::new();
    let mut pending: VecDeque<String> = VecDeque::new();

    loop {
        let line = match pending.pop_front() {
            Some(l) => l,
            None => match rx.recv() {
                Ok(l) => l,
                Err(_) => break, // stdin closed
            },
        };
        if handle_command(&line, &mut eng, &rx, &mut pending) {
            break;
        }
    }
}

/// Handle one command line. Returns true when the loop should exit.
fn handle_command(
    line: &str,
    eng: &mut Engine,
    rx: &mpsc::Receiver<String>,
    pending: &mut VecDeque<String>,
) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return false;
    }
    let mut parts = line.split_whitespace();
    let cmd = parts.next().unwrap_or("");

    match cmd {
        "uci" => {
            println!("id name {}", ENGINE_NAME);
            println!("id author {}", ENGINE_AUTHOR);
            println!("option name Hash type spin default 16 min 1 max 1024");
            println!("uciok");
            flush();
        }
        "isready" => {
            println!("readyok");
            flush();
        }
        "setoption" => {
            // setoption name <name> [value <value>]
            let rest = line["setoption".len()..].trim();
            if let Some(after_name) = rest.strip_prefix("name") {
                let after_name = after_name.trim();
                let (name, value) = match after_name.find("value") {
                    Some(i) => (
                        after_name[..i].trim().to_ascii_lowercase(),
                        Some(after_name[i + "value".len()..].trim().to_string()),
                    ),
                    None => (after_name.trim().to_ascii_lowercase(), None),
                };
                if name == "hash" {
                    if let Some(v) = value {
                        if let Ok(mb) = v.parse::<usize>() {
                            eng.set_hash(mb);
                        }
                    }
                }
                // Other options (Threads, etc.) accepted and ignored: single-threaded.
            }
        }
        "ucinewgame" => {
            eng.board = Board::new_startpos();
            eng.tt.clear();
        }
        "position" => {
            apply_position(line, eng);
        }
        "go" => {
            let limits = parse_go(line);
            run_search(eng, &limits, rx, pending);
            if eng.quit_after_search.load(Ordering::SeqCst) {
                return true;
            }
        }
        "stop" => {
            // Only meaningful during search, which already drains `stop`.
            // When idle: nothing to do.
        }
        "quit" | "exit" => return true,
        // --- debugging extensions (harmless for GUIs) ---
        "d" => {
            print!("{}", eng.board.pretty());
            flush();
        }
        "eval" => {
            let e = crate::eval::evaluate(&eng.board);
            println!("eval (white px): {e}");
            flush();
        }
        "perft" => {
            let depth = parts.next().and_then(|d| d.parse::<u32>().ok()).unwrap_or(4);
            let t = std::time::Instant::now();
            let n = perft(&mut eng.board, depth);
            println!("perft {depth}: {n} nodes in {:?}", t.elapsed());
            flush();
        }
        "bench" => {
            let depth = parts.next().and_then(|d| d.parse::<u8>().ok()).unwrap_or(5);
            run_bench(eng, depth);
        }
        _ => {
            // Unknown commands are ignored per UCI spec ("should" ignore).
        }
    }
    false
}

fn apply_position(line: &str, eng: &mut Engine) {
    // position [fen <fen6> | startpos] [moves <m1> <m2> ...]
    let rest = line["position".len()..].trim().to_string();
    let (mut board, moves_section) = if let Some(fen_part) = rest.strip_prefix("fen") {
        let fen_part = fen_part.trim();
        // FEN is 6 space-separated fields; "moves" may follow.
        let tokens: Vec<&str> = fen_part.split_whitespace().collect();
        let mut fen_fields = Vec::new();
        let mut moves_start = None;
        for (i, t) in tokens.iter().enumerate() {
            if *t == "moves" {
                moves_start = Some(i);
                break;
            }
            fen_fields.push(*t);
        }
        if fen_fields.len() < 4 {
            eprintln!("info string bad FEN in position command");
            return;
        }
        let fen = fen_fields.join(" ");
        let b = match Board::from_fen(&fen) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("info string FEN error: {e}");
                return;
            }
        };
        let moves: Vec<String> = match moves_start {
            Some(i) => tokens[i + 1..].iter().map(|s| s.to_string()).collect(),
            None => Vec::new(),
        };
        (b, moves)
    } else {
        // startpos
        let b = Board::new_startpos();
        let moves: Vec<String> = match rest.find("moves") {
            Some(i) => rest[i + "moves".len()..]
                .split_whitespace()
                .map(|s| s.to_string())
                .collect(),
            None => Vec::new(),
        };
        (b, moves)
    };

    for mstr in moves_section {
        let Some(mv) = Move::from_uci(&mstr) else {
            eprintln!("info string illegal move token: {mstr}");
            break;
        };
        // Validate against legal moves.
        let legal = generate_legal(&mut board);
        if !legal.contains(&mv) {
            eprintln!("info string illegal move: {mstr} in {}", board.to_fen());
            break;
        }
        board.make_move(mv);
    }
    eng.board = board;
}

fn parse_go(line: &str) -> Limits {
    // go [searchmoves ...] [ponder] [wtime N] [btime N] [winc N] [binc N]
    //    [movestogo N] [depth N] [nodes N] [mate N] [movetime N] [infinite]
    let mut lim = Limits::default();
    let toks: Vec<&str> = line.split_whitespace().collect();
    let mut i = 1;
    while i < toks.len() {
        match toks[i] {
            "wtime" => {
                lim.wtime = toks.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "btime" => {
                lim.btime = toks.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "winc" => {
                lim.winc = toks.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "binc" => {
                lim.binc = toks.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "movestogo" => {
                lim.movestogo = toks.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "depth" => {
                lim.depth = toks.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "movetime" => {
                lim.movetime = toks.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "infinite" => {
                lim.infinite = true;
                i += 1;
            }
            // Accepted and ignored: searchmoves, ponder, nodes, mate.
            "searchmoves" => {
                // skip following move tokens
                i += 1;
                while i < toks.len()
                    && !matches!(
                        toks[i],
                        "ponder"
                            | "wtime"
                            | "btime"
                            | "winc"
                            | "binc"
                            | "movestogo"
                            | "depth"
                            | "nodes"
                            | "mate"
                            | "movetime"
                            | "infinite"
                    )
                {
                    i += 1;
                }
            }
            "ponder" | "nodes" | "mate" => {
                i += if toks[i] == "ponder" { 1 } else { 2 };
            }
            _ => {
                i += 1;
            }
        }
    }
    // No time info and no depth and not infinite -> default quick search
    // so bare `go` in a terminal doesn't hang forever.
    if !lim.infinite
        && lim.depth.is_none()
        && lim.movetime.is_none()
        && lim.wtime.is_none()
        && lim.btime.is_none()
    {
        lim.depth = Some(6);
    }
    lim
}

/// Run the search, polling the command channel for stop/quit.
/// Other commands arriving mid-search are queued in `pending`.
fn run_search(
    eng: &mut Engine,
    limits: &Limits,
    rx: &mpsc::Receiver<String>,
    pending: &mut VecDeque<String>,
) {
    let quit_flag = eng.quit_after_search.clone();
    let mut poll = || -> bool {
        loop {
            match rx.try_recv() {
                Ok(cmd) => {
                    let t = cmd.trim();
                    if t == "stop" {
                        return true;
                    }
                    if t == "quit" || t == "exit" {
                        quit_flag.store(true, Ordering::SeqCst);
                        return true;
                    }
                    if t == "isready" {
                        // Defer until search completes.
                        pending.push_back(cmd);
                    } else if t.starts_with("position") || t == "ucinewgame" {
                        pending.push_back(cmd);
                    } else {
                        // Ignore anything else mid-search.
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return false,
                Err(mpsc::TryRecvError::Disconnected) => {
                    quit_flag.store(true, Ordering::SeqCst);
                    return true;
                }
            }
        }
    };

    let res = search_root(&mut eng.board, &mut eng.tt, limits, &mut poll);

    // Flush deferred isready as readyok now (UCI expects prompt reply).
    let mut saw_ready = false;
    let deferred: Vec<String> = pending.drain(..).collect();
    for cmd in deferred {
        if cmd.trim() == "isready" {
            saw_ready = true;
        } else {
            // Re-queue non-isready commands for the main loop (keeps order).
            pending.push_back(cmd);
        }
    }
    void_fen_check();
    if saw_ready {
        println!("readyok");
    }
    match res.best {
        Some(mv) => println!("bestmove {}", mv.to_uci()),
        None => println!("bestmove 0000"),
    }
    flush();
}

fn void_fen_check() {
    // Placeholder to keep helper grouping clear; position validity is
    // enforced in apply_position.
    let _ = STARTPOS_FEN;
}

/// CLI entry for `bench [depth]` (bypasses the UCI loop).
pub fn bench_cli(depth: u8) {
    let mut eng = Engine::new();
    run_bench(&mut eng, depth);
}

/// CLI entry for `search <depth> [fen]` (bypasses the UCI loop).
pub fn search_cli(depth: u8, fen: &str) {
    let mut eng = Engine::new();
    eng.board = Board::from_fen(fen).expect("bad FEN");
    let lim = Limits {
        depth: Some(depth),
        ..Default::default()
    };
    let mut no_poll = || false;
    let res = search_root(&mut eng.board, &mut eng.tt, &lim, &mut no_poll);
    match res.best {
        Some(mv) => println!("bestmove {} score {}", mv.to_uci(), res.score),
        None => println!("bestmove 0000"),
    }
}

fn run_bench(eng: &mut Engine, depth: u8) {
    use std::time::Instant;
    let fens = [
        STARTPOS_FEN,
        "r3k2r/p1ppqpb1/bn2pnp1/3PN3/1p2P3/2N2Q1p/PPPBBPPP/R3K2R w KQkq - 0 1",
        "8/2p5/3p4/KP5r/1R3p1k/8/4P1P1/8 w - - 0 1",
        "r1bqkbnr/pppp1ppp/2n5/4p2Q/2B1P3/8/PPPP1PPP/RNB1K1NR w KQkq - 1 1",
    ];
    let t = Instant::now();
    for fen in fens {
        let mut b = Board::from_fen(fen).unwrap();
        let lim = Limits {
            depth: Some(depth),
            ..Default::default()
        };
        let mut no_poll = || false;
        let res = search_root(&mut b, &mut eng.tt, &lim, &mut no_poll);
        println!(
            "bench {} -> bestmove {} score {}",
            fen,
            res.best.map(|m| m.to_uci()).unwrap_or_else(|| "0000".into()),
            res.score
        );
    }
    println!("bench depth {depth}: {:?} total", t.elapsed());
    flush();
}

fn flush() {
    let _ = std::io::stdout().flush();
}
