//! Peer-to-peer protocol between two game instances.
//!
//! No central server. One instance is the host (it listens),
//! the other is the joiner (it connects). After the handshake
//! both sides are symmetric: each owns its own local room,
//! generates the same sequence from the same seed, and only
//! exchanges button presses.
//!
//! Wire format (std only, no crates):
//!   [4-byte big-endian length][UTF-8 JSON payload]
//! The length prefix is what makes TCP usable as messages:
//! TCP is a byte stream, so without it a reader cannot tell
//! where one JSON object ends and the next begins.
//!
//! Messages (all JSON, one `type` field), in order:
//!   Host -> Joiner: {"type":"hello","seed":123,"n":2,
//!                    "total_trials":20,"trial_ms":3000}
//!   Host -> Joiner: {"type":"ping","seq":0}  (x3)
//!   Joiner -> Host: {"type":"pong","seq":0}  (immediate reply)
//!   Host -> Joiner: {"type":"start","one_way_ms":3}
//!   Either -> Peer: {"type":"input","trial":3,
//!                    "position":true,"audio":false,
//!                    "pressed_at_ms":6123}
//!
//! Clock sync (v0.2.2): the host measures the round-trip time
//! with 3 ping/pong exchanges and takes the minimum RTT — the
//! minimum is the least-queued sample, so the best estimate
//! of the true network delay. `one_way_ms` is RTT/2. The
//! host starts its clock when it sends Start; the joiner
//! starts its clock `one_way_ms` in the past on receipt, so
//! both clocks agree to within a few ms instead of lagging
//! by a full one-way trip. If the ping exchange fails, the
//! host falls back to `one_way_ms = 0` and the game still
//! starts (same as v0.2.0 behaviour).
//!
//! Timestamped inputs (v0.2.2): `pressed_at_ms` is the
//! milliseconds since game start on the *sender's* clock at
//! the moment the sender's local instance received the press
//! (localhost, ~1ms after the browser click). The receiver
//! judges the press by that timestamp, not by arrival time:
//! the trial must match `pressed_at_ms / trial_ms`, and the
//! press must not arrive implausibly late or claim a time
//! implausibly in the future (see `server::valid_press`).
//! That removes network latency from the scoring decision —
//! a press made on time but delivered late still counts,
//! and a press made late cannot be backdated. Timestamps
//! are self-reported, so this is fairness for a friendly
//! game, not anti-cheat for a hostile one: the receiver's
//! arrival-time check bounds how far a timestamp can lie.

use crate::server::{parse_bool_field, parse_string_field, parse_u64_field, parse_usize_field};
use std::io::{self, Read, Write};
use std::net::TcpStream;

/// Max message size: 64 KiB. Our messages are under 200 bytes;
/// anything larger is a protocol error, not a game message.
const MAX_MSG: usize = 64 * 1024;

pub fn send_msg(stream: &mut TcpStream, json: &str) -> io::Result<()> {
    let bytes = json.as_bytes();
    let len = (bytes.len() as u32).to_be_bytes();
    stream.write_all(&len)?;
    stream.write_all(bytes)?;
    stream.flush()
}

pub fn recv_msg(stream: &mut TcpStream) -> io::Result<String> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_MSG {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bad message length: {len}"),
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "message was not UTF-8")
    })
}

pub fn hello_msg(seed: u64, n: usize, total_trials: usize, trial_ms: u64) -> String {
    format!(
        "{{\"type\":\"hello\",\"seed\":{seed},\"n\":{n},\"total_trials\":{total_trials},\"trial_ms\":{trial_ms}}}"
    )
}

pub fn ping_msg(seq: u64) -> String {
    format!("{{\"type\":\"ping\",\"seq\":{seq}}}")
}

pub fn pong_msg(seq: u64) -> String {
    format!("{{\"type\":\"pong\",\"seq\":{seq}}}")
}

pub fn start_msg(one_way_ms: u64) -> String {
    format!("{{\"type\":\"start\",\"one_way_ms\":{one_way_ms}}}")
}

pub fn input_msg(trial: usize, position: bool, audio: bool, pressed_at_ms: u64) -> String {
    format!(
        "{{\"type\":\"input\",\"trial\":{trial},\"position\":{position},\"audio\":{audio},\"pressed_at_ms\":{pressed_at_ms}}}"
    )
}

/// (seed, n) from a Hello message, or None if malformed.
pub fn parse_hello(msg: &str) -> Option<(u64, usize)> {
    if parse_string_field(msg, "type").as_deref() != Some("hello") {
        return None;
    }
    let seed = parse_u64_field(msg, "seed")?;
    let n = parse_usize_field(msg, "n")?;
    Some((seed, n))
}

pub fn is_start(msg: &str) -> bool {
    parse_string_field(msg, "type").as_deref() == Some("start")
}

/// one_way_ms from a Start message, or None if malformed.
/// Missing field means an old (v0.2.0/0.2.1) sender: treat
/// as 0 rather than failing the handshake.
pub fn parse_start(msg: &str) -> Option<u64> {
    if !is_start(msg) {
        return None;
    }
    Some(parse_u64_field(msg, "one_way_ms").unwrap_or(0))
}

/// seq from a Ping message, or None.
pub fn parse_ping(msg: &str) -> Option<u64> {
    if parse_string_field(msg, "type").as_deref() != Some("ping") {
        return None;
    }
    parse_u64_field(msg, "seq")
}

/// seq from a Pong message, or None.
pub fn parse_pong(msg: &str) -> Option<u64> {
    if parse_string_field(msg, "type").as_deref() != Some("pong") {
        return None;
    }
    parse_u64_field(msg, "seq")
}

/// (trial, position, audio, pressed_at_ms) from an Input
/// message, or None. A message without pressed_at_ms is
/// malformed in v0.2.2 (old senders are not supported).
pub fn parse_input(msg: &str) -> Option<(usize, bool, bool, u64)> {
    if parse_string_field(msg, "type").as_deref() != Some("input") {
        return None;
    }
    let trial = parse_usize_field(msg, "trial")?;
    let position = parse_bool_field(msg, "position")?;
    let audio = parse_bool_field(msg, "audio")?;
    let pressed_at_ms = parse_u64_field(msg, "pressed_at_ms")?;
    Some((trial, position, audio, pressed_at_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn hello_ping_start_input_round_trip() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let msg = recv_msg(&mut stream).unwrap();
            assert_eq!(parse_hello(&msg), Some((4242, 2)));
            // Host pings, joiner answers immediately.
            let msg = recv_msg(&mut stream).unwrap();
            assert_eq!(parse_ping(&msg), Some(0));
            send_msg(&mut stream, &pong_msg(0)).unwrap();
            let msg = recv_msg(&mut stream).unwrap();
            assert!(is_start(&msg));
            assert_eq!(parse_start(&msg), Some(3));
            let msg = recv_msg(&mut stream).unwrap();
            assert_eq!(parse_input(&msg), Some((3, true, false, 6123)));
            send_msg(&mut stream, &input_msg(3, false, true, 6100)).unwrap();
        });
        let mut stream = TcpStream::connect(addr).unwrap();
        send_msg(&mut stream, &hello_msg(4242, 2, 20, 3000)).unwrap();
        send_msg(&mut stream, &ping_msg(0)).unwrap();
        let pong = recv_msg(&mut stream).unwrap();
        assert_eq!(parse_pong(&pong), Some(0));
        send_msg(&mut stream, &start_msg(3)).unwrap();
        send_msg(&mut stream, &input_msg(3, true, false, 6123)).unwrap();
        let back = recv_msg(&mut stream).unwrap();
        assert_eq!(parse_input(&back), Some((3, false, true, 6100)));
        handle.join().unwrap();
    }

    #[test]
    fn malformed_messages_rejected() {
        assert_eq!(parse_hello("{\"type\":\"start\"}"), None);
        assert_eq!(parse_hello("{\"type\":\"hello\",\"seed\":1}"), None);
        assert!(!is_start("{\"type\":\"hello\"}"));
        // Start without one_way_ms is accepted as 0 (old sender).
        assert_eq!(parse_start("{\"type\":\"start\"}"), Some(0));
        assert_eq!(parse_start("{\"type\":\"start\",\"one_way_ms\":7}"), Some(7));
        assert_eq!(parse_ping("{\"type\":\"ping\"}"), None);
        assert_eq!(parse_pong("{\"type\":\"pong\",\"seq\":1}"), Some(1));
        // Input without a timestamp is malformed now.
        assert_eq!(parse_input("{\"type\":\"input\",\"trial\":1}"), None);
        assert_eq!(
            parse_input("{\"type\":\"input\",\"trial\":1,\"position\":true,\"audio\":false}"),
            None
        );
        assert_eq!(
            parse_input("{\"type\":\"input\",\"trial\":1,\"position\":true,\"audio\":false,\"pressed_at_ms\":100}"),
            Some((1, true, false, 100))
        );
    }
}
