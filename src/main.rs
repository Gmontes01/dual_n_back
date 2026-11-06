//! Binary: two game instances, no central server (v0.2.0,
//! solo mode added in v0.2.1, timestamped inputs + clock
//! sync added in v0.2.2).
//!
//! Each instance is a complete game: it serves its own web UI
//! on localhost (the book's ThreadPool server, kept for that
//! local job only) and, in host/join mode, it talks to the
//! other instance over a direct TCP peer link (`p2p.rs`).
//! Solo mode uses no network at all: one local room, one
//! player, clock starts immediately.
//!
//! ```sh
//! # Solo practice:
//! cargo run -- solo
//! # UI: http://127.0.0.1:7878
//!
//! # Terminal 1 (host): listens for a peer on :7879,
//! # serves its own UI on http://127.0.0.1:7878
//! cargo run -- host
//!
//! # Terminal 2 (joiner): connects to the host,
//! # serves its own UI on http://127.0.0.1:7880
//! cargo run -- join 127.0.0.1:7879 --web-port 7880
//! ```
//!
//! On two machines on the same Wi-Fi, the joiner uses the
//! host's LAN IP instead of 127.0.0.1. Over the internet the
//! host's router must forward the P2P port (or use a NAT
//! traversal library instead -- see README.md).
//!
//! Handshake: host generates a seed, sends Hello (seed + n),
//! measures the round-trip time with 3 ping/pong exchanges,
//! then sends Start with `one_way_ms` (min RTT / 2). Both
//! sides build the same sequence from the seed. The host
//! starts its clock when it sends Start; the joiner starts
//! its clock `one_way_ms` in the past, so the clocks agree
//! instead of lagging by a full one-way trip. After that
//! only timestamped button presses cross the link
//! (`pressed_at_ms`, ms since game start on the sender's
//! clock), judged by timestamp, not arrival time. A
//! dedicated reader thread owns the peer socket's read half;
//! web workers own the write half (via `peer_tx`) to forward
//! your presses.
//!
//! The old central-server mode (one server, two browser tabs,
//! `/api/create` + `/api/join`) is removed in v0.2.0. That is
//! the point of this conversion: no process has to stay
//! running in the background for two players to play.

use dual_nback_trpl::p2p::{
    hello_msg, input_msg, parse_hello, parse_input, parse_ping, parse_pong, parse_start,
    ping_msg, pong_msg, recv_msg, send_msg, start_msg,
};
use dual_nback_trpl::server::{
    parse_bool_field, parse_string_field, parse_usize_field, query_param, ServerState,
    DEFAULT_N, TOTAL_TRIALS, TRIAL_MS,
};
use std::{
    env,
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

/// Everything the local web workers and the P2P reader share.
struct App {
    state: Arc<Mutex<ServerState>>,
    /// Write half of the peer link. Web workers lock this to
    /// forward your button presses; the reader thread never
    /// touches it (it owns a separate `try_clone` read half).
    peer_tx: Arc<Mutex<Option<TcpStream>>>,
    mode: String,
    peer_addr: String,
    web_port: u16,
    room: String,
    your_token: String,
}

fn usage() -> ! {
    eprintln!("Usage:");
    eprintln!("  dual_nback_trpl solo [--web-port 7878] [--n 2] [--seed 123]");
    eprintln!("  dual_nback_trpl host [--web-port 7878] [--p2p-port 7879] [--n 2]");
    eprintln!("  dual_nback_trpl join <host-ip:p2p-port> [--web-port 7880]");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  cargo run -- solo");
    eprintln!("  cargo run -- host");
    eprintln!("  cargo run -- join 127.0.0.1:7879 --web-port 7880");
    std::process::exit(2);
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].clone())
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        usage();
    }
    let mode = args[0].clone();
    if mode != "host" && mode != "join" && mode != "solo" {
        usage();
    }
    let web_port: u16 = flag_value(&args, "--web-port")
        .and_then(|v| v.parse().ok())
        .unwrap_or(if mode == "join" { 7880 } else { 7878 });
    let n: usize = flag_value(&args, "--n")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_N);

    let state = Arc::new(Mutex::new(ServerState::new()));
    let peer_tx: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));

    // --- Solo: no network, no waiting. Same engine, same
    // scoring, just one player. `--seed` replays an exact
    // sequence (useful for practice); without it the seed
    // comes from the clock.
    let (room, your_token) = if mode == "solo" {
        let seed = flag_value(&args, "--seed")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0x12345678)
            });
        let (room, your_token) = {
            let mut locked = state.lock().unwrap();
            locked.create_solo_room(n, seed)
        };
        println!("Solo game started (n={n}, seed {seed}). No peer needed.");
        println!("Replay this exact game later with: cargo run -- solo --seed {seed}");
        (room, your_token)
    } else if mode == "host" {
        // --- P2P handshake (blocks until the peer is connected) ---
        let p2p_port: u16 = flag_value(&args, "--p2p-port")
            .and_then(|v| v.parse().ok())
            .unwrap_or(7879);
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x12345678);
        let (room, your_token, _) = {
            let mut locked = state.lock().unwrap();
            locked.create_p2p_room(n, seed)
        };
        println!("Hosting. Waiting for a peer on 0.0.0.0:{p2p_port} ...");
        println!("Joiner runs: cargo run -- join <this-machine-ip>:{p2p_port} --web-port 7880");
        let listener = TcpListener::bind(format!("0.0.0.0:{p2p_port}"))
            .unwrap_or_else(|e| {
                eprintln!("Could not listen on :{p2p_port}: {e}");
                std::process::exit(1);
            });
        let (mut stream, peer_addr) = listener.accept().unwrap_or_else(|e| {
            eprintln!("Accept failed: {e}");
            std::process::exit(1);
        });
        println!("Peer connected from {peer_addr}. Starting game.");
        send_msg(
            &mut stream,
            &hello_msg(seed, n, TOTAL_TRIALS, TRIAL_MS),
        )
        .unwrap();
        // Clock sync: measure RTT with 3 ping/pong exchanges
        // (minimum RTT is the least-queued sample), then tell
        // the joiner the one-way delay so it can start its
        // clock that far in the past.
        let one_way_ms = measure_one_way(&mut stream);
        send_msg(&mut stream, &start_msg(one_way_ms)).unwrap();
        {
            let mut locked = state.lock().unwrap();
            locked.set_peer_connected(&room, true);
            locked.start_room(&room);
        }
        spawn_peer_reader(stream, Arc::clone(&state), Arc::clone(&peer_tx), room.clone());
        (room, your_token)
    } else {
        let host_addr = args.get(1).cloned().unwrap_or_else(|| {
            eprintln!("join needs a host address, e.g. join 192.168.1.42:7879");
            usage();
        });
        println!("Connecting to host at {host_addr} ...");
        let mut stream = TcpStream::connect(&host_addr).unwrap_or_else(|e| {
            eprintln!("Could not connect to {host_addr}: {e}");
            eprintln!("Is the host running, and is the port forwarded / on the same Wi-Fi?");
            std::process::exit(1);
        });
        let hello = recv_msg(&mut stream).unwrap_or_else(|e| {
            eprintln!("Failed to read Hello from host: {e}");
            std::process::exit(1);
        });
        let (seed, host_n) = parse_hello(&hello).unwrap_or_else(|| {
            eprintln!("Host sent a malformed Hello: {hello}");
            std::process::exit(1);
        });
        let (room, your_token, _) = {
            let mut locked = state.lock().unwrap();
            locked.create_p2p_room(host_n, seed)
        };
        // The host pings first (clock sync), then sends Start.
        // Answer every ping immediately, and start when Start
        // arrives — starting the clock one_way_ms in the past
        // so it agrees with the host's clock.
        let one_way_ms = loop {
            let msg = recv_msg(&mut stream).unwrap_or_else(|e| {
                eprintln!("Failed while waiting for Start from host: {e}");
                std::process::exit(1);
            });
            if let Some(seq) = parse_ping(&msg) {
                let _ = send_msg(&mut stream, &pong_msg(seq));
                continue;
            }
            match parse_start(&msg) {
                Some(one_way) => break one_way,
                None => {
                    eprintln!("Expected Start from host, got: {msg}");
                    std::process::exit(1);
                }
            }
        };
        {
            let mut locked = state.lock().unwrap();
            locked.set_peer_connected(&room, true);
            locked.start_room_offset(&room, one_way_ms);
        }
        println!("Connected. Game started (n={host_n}, clock adjusted back {one_way_ms} ms).");
        spawn_peer_reader(stream, Arc::clone(&state), Arc::clone(&peer_tx), room.clone());
        (room, your_token)
    };

    // The writer half is a clone of the same socket, stored in
    // peer_tx by spawn_peer_reader. The reader thread owns the
    // original handle; web workers write through peer_tx.
    let app = Arc::new(App {
        state,
        peer_tx,
        mode: mode.clone(),
        peer_addr: if mode == "solo" {
            "solo — no peer".to_string()
        } else if mode == "host" {
            "peer connected".to_string()
        } else {
            args.get(1).cloned().unwrap_or_default()
        },
        web_port,
        room,
        your_token,
    });

    // --- Local web UI server (Tokio async tasks, localhost only) ---
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{web_port}"))
        .await
        .unwrap_or_else(|e| {
            eprintln!("Could not listen on 127.0.0.1:{web_port}: {e}");
            eprintln!("If you are running two instances on one machine, give the joiner a different --web-port.");
            std::process::exit(1);
        });
    println!("{mode} UI at http://127.0.0.1:{web_port} — open that in your browser.");
    if mode != "solo" {
        println!("Your opponent opens their own instance's UI on their own machine.");
    }
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(pair) => pair,
            Err(_) => continue,
        };
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            handle_connection(stream, app).await;
        });
    }
}

/// Measure the one-way network delay with 3 ping/pong
/// exchanges and return min-RTT / 2 (v0.2.2). The minimum
/// RTT is the least-queued sample, so the best estimate of
/// the true delay. Returns 0 if the exchange fails — the
/// game still starts, just without clock-sync correction.
/// Uses a 2s read timeout for the pings only; the timeout
/// is cleared before the socket is handed to the reader
/// thread, which blocks indefinitely on purpose.
fn measure_one_way(stream: &mut TcpStream) -> u64 {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut best: Option<u64> = None;
    for seq in 0..3u64 {
        let t0 = std::time::Instant::now();
        if send_msg(stream, &ping_msg(seq)).is_err() {
            break;
        }
        match recv_msg(stream) {
            Ok(msg) if parse_pong(&msg) == Some(seq) => {
                let rtt = t0.elapsed().as_millis() as u64;
                best = Some(best.map_or(rtt, |b: u64| b.min(rtt)));
            }
            _ => break,
        }
    }
    let _ = stream.set_read_timeout(None);
    match best {
        Some(rtt) => {
            println!("Clock sync: min RTT {rtt} ms over 3 pings, one-way ~{} ms.", rtt / 2);
            rtt / 2
        }
        None => {
            println!("Clock sync: ping failed, assuming one-way 0 ms.");
            0
        }
    }
}

/// One dedicated thread reads peer Input messages forever.
/// On disconnect it marks the peer offline; the local game
/// keeps running to trial 20 either way.
///
/// The socket is split with `try_clone`: the reader thread
/// owns the original handle, and a clone is stored in
/// `peer_tx` for the web workers to write your presses on.
/// Both handles refer to the same TCP connection, so reads
/// and writes do not interfere with each other.
fn spawn_peer_reader(
    mut stream: TcpStream,
    state: Arc<Mutex<ServerState>>,
    peer_tx: Arc<Mutex<Option<TcpStream>>>,
    room: String,
) {
    if let Ok(writer) = stream.try_clone() {
        *peer_tx.lock().unwrap() = Some(writer);
    }
    thread::spawn(move || loop {
        match recv_msg(&mut stream) {
            Ok(msg) => {
                if let Some((trial, position, audio, pressed_at_ms)) = parse_input(&msg) {
                    let mut locked = state.lock().unwrap();
                    locked.record_peer_response(&room, trial, position, audio, pressed_at_ms);
                }
                // Unknown message types are ignored, not fatal.
            }
            Err(_) => {
                let mut locked = state.lock().unwrap();
                locked.set_peer_connected(&room, false);
                break;
            }
        }
    });
}

async fn respond<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    status: &str,
    content_type: &str,
    body: &str,
) {
    let response = format!(
        "{status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = writer.write_all(response.as_bytes()).await;
    let _ = writer.flush().await;
}

async fn respond_json<W: AsyncWriteExt + Unpin>(writer: &mut W, status: &str, body: &str) {
    respond(writer, status, "application/json", body).await;
}

async fn serve_file<W: AsyncWriteExt + Unpin>(writer: &mut W, path: &str, content_type: &str) {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => respond(writer, "HTTP/1.1 200 OK", content_type, &contents).await,
        Err(_) => respond(
            writer,
            "HTTP/1.1 404 NOT FOUND",
            "text/html",
            "<h1>404</h1><p>File not found. Run from the project root.</p>",
        )
        .await,
    }
}

async fn handle_connection(mut stream: tokio::net::TcpStream, app: Arc<App>) {
    let (reader, mut writer) = stream.split();
    let mut reader = BufReader::new(reader);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await.is_err() || request_line.trim().is_empty() {
        return;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let full_path = parts.next().unwrap_or("");
    let (path, query) = match full_path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (full_path, ""),
    };

    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                if line.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.trim().eq_ignore_ascii_case("content-length") {
                        content_length = value.trim().parse().unwrap_or(0);
                    }
                }
            }
            Err(_) => return,
        }
    }

    let mut body = String::new();
    if content_length > 0 {
        let mut buf = vec![0u8; content_length];
        if reader.read_exact(&mut buf).await.is_ok() {
            body = String::from_utf8_lossy(&buf).into_owned();
        }
    }
    drop(reader);

    match (method, path) {
        ("GET", "/") => serve_file(&mut writer, "index.html", "text/html").await,
        ("GET", "/app.js") => serve_file(&mut writer, "app.js", "text/javascript").await,
        ("GET", "/api/info") => {
            let json = format!(
                "{{\"mode\":\"{}\",\"peer\":\"{}\",\"web_port\":{},\"room\":\"{}\",\"token\":\"{}\"}}",
                app.mode, app.peer_addr, app.web_port, app.room, app.your_token
            );
            respond_json(&mut writer, "HTTP/1.1 200 OK", &json).await;
        }
        ("GET", "/api/state") => {
            let room = query_param(query, "room").unwrap_or(&app.room);
            let token = query_param(query, "token").unwrap_or(&app.your_token);
            let state_result = {
                let locked = app.state.lock().unwrap();
                locked.state_json(room, token)
            };
            match state_result {
                Some(json) => respond_json(&mut writer, "HTTP/1.1 200 OK", &json).await,
                None => respond_json(
                    &mut writer,
                    "HTTP/1.1 404 NOT FOUND",
                    r#"{"error":"room not found or bad token"}"#,
                )
                .await,
            }
        }
        ("POST", "/api/respond") => {
            let room = parse_string_field(&body, "room").unwrap_or_else(|| app.room.clone());
            let token = parse_string_field(&body, "token").unwrap_or_else(|| app.your_token.clone());
            let trial = parse_usize_field(&body, "trial").unwrap_or(usize::MAX);
            let position = parse_bool_field(&body, "position").unwrap_or(false);
            let audio = parse_bool_field(&body, "audio").unwrap_or(false);
            let pressed_at_ms = {
                let locked = app.state.lock().unwrap();
                locked.elapsed_ms(&room).unwrap_or(0)
            };
            let ok = {
                let mut locked = app.state.lock().unwrap();
                locked.record_response(&room, &token, trial, position, audio, pressed_at_ms)
            };
            if ok {
                let mut peer_tx_lock = app.peer_tx.lock().unwrap();
                if let Some(tx) = peer_tx_lock.as_mut() {
                    let _ = send_msg(tx, &input_msg(trial, position, audio, pressed_at_ms));
                }
            }
            let json = format!("{{\"ok\":{ok}}}");
            respond_json(&mut writer, "HTTP/1.1 200 OK", &json).await;
        }
        ("GET", "/sleep") => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            respond(&mut writer, "HTTP/1.1 200 OK", "text/plain", "slept 5s").await;
        }
        ("GET", "/favicon.ico") => respond(
            &mut writer,
            "HTTP/1.1 404 NOT FOUND",
            "text/plain",
            "",
        )
        .await,
        _ => respond(
            &mut writer,
            "HTTP/1.1 404 NOT FOUND",
            "text/html",
            "<h1>404</h1><p>Unknown route.</p>",
        )
        .await,
    }
}
