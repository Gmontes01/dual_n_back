//! Game state shared by all ThreadPool workers.
//!
//! This is the one deliberate departure from the book's
//! `hello.html` server: the book's handler is stateless, but a
//! 1v1 game needs shared state. The shape is the same idea the
//! book teaches in Chapter 16 -- `Arc<Mutex<T>>` -- applied to
//! a `ServerState` holding every room. `main.rs` clones the
//! `Arc` into each `pool.execute` closure.
//!
//! v0.2.0 (P2P): the central-server lobby is gone. Each game
//! instance owns one local room (`create_p2p_room`) with
//! players[0] = you, players[1] = your peer. The peer link
//! (`p2p.rs`) forwards your button presses to the peer and
//! records the peer's presses with `record_peer_response`.
//! Both instances generate the same sequence from the same
//! seed, so both can score both players locally.
//!
//! Timing is derived, not ticked: there is no background
//! thread advancing trials. `Room::current_trial` computes the
//! trial from `started_at.elapsed()`, so any worker can answer
//! `/api/state` correctly at any moment. That keeps the
//! ThreadPool model intact -- workers only run when a request
//! arrives.
//!
//! No serde: this crate has no dependencies, so JSON is built
//! and parsed by hand for the small, fixed shapes we use.
//! If you later move to Axum, replace this module with serde
//! structs and keep `engine.rs` untouched.

use crate::engine::{generate_sequence, matches, score_trial, Stimulus};
use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// One stimulus every 3 seconds. 20 trials = a 60-second game.
pub const TRIAL_MS: u64 = 3000;
pub const TOTAL_TRIALS: usize = 20;
pub const DEFAULT_N: usize = 2;

/// Grace for a press whose trial tag crossed a boundary
/// (v0.2.2). The browser polls state every 250ms, so the
/// trial it shows can lag the server by up to 250ms: a press
/// made in the first 250ms of a new trial may still carry
/// the previous trial's tag, because the player was still
/// looking at the previous stimulus. Accept it as the
/// previous trial's press.
pub const GRACE_MS: u64 = 250;

/// Max believable delay between a press being made and it
/// arriving at the receiver (v0.2.2). A press that arrives
/// more than a second after its timestamp is too stale to
/// trust — the trial it claims is long over.
pub const MAX_LATENCY_MS: u64 = 1000;

/// Judge a press by its timestamp, not by arrival time
/// (v0.2.2). `pressed_at_ms` is ms since game start on the
/// sender's clock; `arrival_ms` is ms since game start on
/// the receiver's clock at receipt (clocks are synced to
/// within a few ms by the ping exchange, see `p2p.rs`).
/// Returns false if the timestamp does not belong to the
/// claimed trial, if it is past the end of the game, if it
/// arrived implausibly late, or if it claims a time too far
/// in the receiver's future (clock skew beyond GRACE_MS,
/// or a fabricated timestamp).
pub fn valid_press(trial: usize, pressed_at_ms: u64, arrival_ms: u64) -> bool {
    if pressed_at_ms >= TRIAL_MS * TOTAL_TRIALS as u64 {
        return false;
    }
    let trial_from_ts = (pressed_at_ms / TRIAL_MS) as usize;
    let in_trial = trial_from_ts == trial;
    let boundary = trial_from_ts >= 1
        && trial == trial_from_ts - 1
        && pressed_at_ms % TRIAL_MS < GRACE_MS;
    if !in_trial && !boundary {
        return false;
    }
    if arrival_ms >= pressed_at_ms {
        arrival_ms - pressed_at_ms <= MAX_LATENCY_MS
    } else {
        pressed_at_ms - arrival_ms <= GRACE_MS
    }
}

const CODE_LETTERS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";

#[derive(Clone)]
pub struct Player {
    pub token: String,
    /// trial -> (position pressed, audio pressed), merged with OR
    /// so a client that re-sends cumulative flags cannot un-press.
    pub responses: HashMap<usize, (bool, bool)>,
}

pub struct Room {
    pub code: String,
    pub n: usize,
    pub sequence: Vec<Stimulus>,
    pub players: Vec<Player>,
    pub started_at: Option<Instant>,
    /// P2P only: true while the peer TCP link is up. The room
    /// always has 2 players (you + peer), so this is the live
    /// connection flag, not the player count.
    pub peer_connected: bool,
}

impl Room {
    pub fn status(&self) -> &'static str {
        match self.started_at {
            None => "waiting",
            Some(started) => {
                if started.elapsed().as_millis() as u64 >= TRIAL_MS * TOTAL_TRIALS as u64 {
                    "finished"
                } else {
                    "playing"
                }
            }
        }
    }

    /// Index of the trial in progress, or None when waiting/finished.
    pub fn current_trial(&self) -> Option<usize> {
        let started = self.started_at?;
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let trial = (elapsed_ms / TRIAL_MS) as usize;
        if trial >= TOTAL_TRIALS {
            None
        } else {
            Some(trial)
        }
    }

    pub fn trial_remaining_ms(&self) -> u64 {
        let Some(started) = self.started_at else {
            return 0;
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        let total_ms = TRIAL_MS * TOTAL_TRIALS as u64;
        if elapsed_ms >= total_ms {
            0
        } else {
            TRIAL_MS - (elapsed_ms % TRIAL_MS)
        }
    }

    /// Number of trials that are fully over, and therefore scored.
    pub fn completed_trials(&self) -> usize {
        match self.status() {
            "finished" => TOTAL_TRIALS,
            "playing" => self.current_trial().unwrap_or(0),
            _ => 0,
        }
    }
}

/// (score, hits, false alarms, misses) for one player over the
/// completed trials. Recomputed on demand -- 20 trials is cheap,
/// and recomputing means scoring can never drift from responses.
pub fn player_stats(room: &Room, player: &Player) -> (i32, u32, u32, u32) {
    let mut score = 0;
    let mut hits = 0;
    let mut false_alarms = 0;
    let mut misses = 0;
    for t in 0..room.completed_trials() {
        let (exp_pos, exp_audio) = matches(&room.sequence, t, room.n);
        let (pressed_pos, pressed_audio) = player.responses.get(&t).copied().unwrap_or((false, false));
        score += score_trial(exp_pos, exp_audio, pressed_pos, pressed_audio);
        if exp_pos && pressed_pos {
            hits += 1;
        }
        if exp_audio && pressed_audio {
            hits += 1;
        }
        if !exp_pos && pressed_pos {
            false_alarms += 1;
        }
        if !exp_audio && pressed_audio {
            false_alarms += 1;
        }
        if exp_pos && !pressed_pos {
            misses += 1;
        }
        if exp_audio && !pressed_audio {
            misses += 1;
        }
    }
    (score, hits, false_alarms, misses)
}

pub struct ServerState {
    pub rooms: HashMap<String, Room>,
    counter: u64,
}

impl ServerState {
    pub fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x12345678);
        Self {
            rooms: HashMap::new(),
            counter: seed,
        }
    }

    fn next_u64(&mut self) -> u64 {
        // SplitMix64, same family as the engine RNG.
        self.counter = self.counter.wrapping_add(0x9e3779b97f4a7c15);
        let mut x = self.counter;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
        x ^ (x >> 31)
    }

    fn new_code(&mut self) -> String {
        loop {
            let mut code = String::with_capacity(4);
            for _ in 0..4 {
                let idx = (self.next_u64() % CODE_LETTERS.len() as u64) as usize;
                code.push(CODE_LETTERS[idx] as char);
            }
            if !self.rooms.contains_key(&code) {
                return code;
            }
        }
    }

    fn new_token(&mut self) -> String {
        format!("{:016x}{:016x}", self.next_u64(), self.next_u64())
    }

    /// Create a room as player 1. Returns (room code, token).
    pub fn create_room(&mut self, n: usize) -> (String, String) {
        let n = n.clamp(1, 3);
        let code = self.new_code();
        let token = self.new_token();
        let seed = self.next_u64();
        let sequence = generate_sequence(seed, n, TOTAL_TRIALS);
        let room = Room {
            code: code.clone(),
            n,
            sequence,
            players: vec![Player {
                token: token.clone(),
                responses: HashMap::new(),
            }],
            started_at: None,
            peer_connected: false,
        };
        self.rooms.insert(code.clone(), room);
        (code, token)
    }

    /// P2P room: one local room per game instance, built from
    /// a shared seed so both instances generate the identical
    /// sequence. players[0] is always you, players[1] is
    /// always the peer, on both sides. Returns
    /// (code, your_token, peer_token). The game does NOT start
    /// here: the host starts it when it sends Start, the
    /// joiner when it receives Start (see `start_room`).
    pub fn create_p2p_room(&mut self, n: usize, seed: u64) -> (String, String, String) {
        let n = n.clamp(1, 3);
        let your_token = self.new_token();
        let peer_token = self.new_token();
        let sequence = generate_sequence(seed, n, TOTAL_TRIALS);
        let room = Room {
            code: "P2P".to_string(),
            n,
            sequence,
            players: vec![
                Player {
                    token: your_token.clone(),
                    responses: HashMap::new(),
                },
                Player {
                    token: peer_token.clone(),
                    responses: HashMap::new(),
                },
            ],
            started_at: None,
            peer_connected: false,
        };
        self.rooms.insert("P2P".to_string(), room);
        ("P2P".to_string(), your_token, peer_token)
    }

    /// Solo room: one player, no peer, no network. The clock
    /// starts immediately on creation, so `cargo run -- solo`
    /// is playing within a second of starting. Returns
    /// (code, your_token). Scoring is identical to 1v1 — the
    /// same sequence generator, same +10 / -5 table — so a
    /// solo score is directly comparable to a 1v1 score.
    pub fn create_solo_room(&mut self, n: usize, seed: u64) -> (String, String) {
        let n = n.clamp(1, 3);
        let your_token = self.new_token();
        let sequence = generate_sequence(seed, n, TOTAL_TRIALS);
        let room = Room {
            code: "SOLO".to_string(),
            n,
            sequence,
            players: vec![Player {
                token: your_token.clone(),
                responses: HashMap::new(),
            }],
            started_at: Some(Instant::now()),
            peer_connected: false,
        };
        self.rooms.insert("SOLO".to_string(), room);
        ("SOLO".to_string(), your_token)
    }

    /// Start the P2P room's clock. Returns false if already
    /// started or the room does not exist.
    pub fn start_room(&mut self, code: &str) -> bool {
        self.start_room_offset(code, 0)
    }

    /// Start the room's clock `back_ms` in the past (v0.2.2).
    /// The joiner uses this with the measured one-way delay
    /// from the ping exchange, so its clock agrees with the
    /// host's instead of lagging by a full one-way trip.
    /// Returns false if already started or no such room.
    pub fn start_room_offset(&mut self, code: &str, back_ms: u64) -> bool {
        let Some(room) = self.rooms.get_mut(code) else {
            return false;
        };
        if room.started_at.is_some() {
            return false;
        }
        let now = Instant::now();
        room.started_at = Some(
            now.checked_sub(std::time::Duration::from_millis(back_ms))
                .unwrap_or(now),
        );
        true
    }

    /// Milliseconds since the room's clock started, or None
    /// if the room does not exist or has not started. Used
    /// to timestamp a press at the moment the local instance
    /// receives it (v0.2.2).
    pub fn elapsed_ms(&self, code: &str) -> Option<u64> {
        let room = self.rooms.get(code)?;
        room.started_at
            .map(|s| s.elapsed().as_millis() as u64)
    }

    pub fn set_peer_connected(&mut self, code: &str, connected: bool) {
        if let Some(room) = self.rooms.get_mut(code) {
            room.peer_connected = connected;
        }
    }

    /// Record a button press received from the peer over the
    /// P2P link (player index 1), judged by its timestamp
    /// (v0.2.2). A press made on time but delivered late
    /// still counts, because scoring is by `pressed_at_ms`,
    /// not arrival time; a press made late cannot be
    /// backdated, because `valid_press` checks the timestamp
    /// against the claimed trial and against arrival time.
    /// Future trials and waiting games are rejected.
    pub fn record_peer_response(
        &mut self,
        code: &str,
        trial: usize,
        pressed_pos: bool,
        pressed_audio: bool,
        pressed_at_ms: u64,
    ) -> bool {
        let Some(room) = self.rooms.get_mut(code) else {
            return false;
        };
        if room.status() == "waiting" {
            return false;
        }
        if trial > room.completed_trials() {
            return false;
        }
        let arrival_ms = room
            .started_at
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0);
        if !valid_press(trial, pressed_at_ms, arrival_ms) {
            return false;
        }
        let entry = room.players[1]
            .responses
            .entry(trial)
            .or_insert((false, false));
        entry.0 |= pressed_pos;
        entry.1 |= pressed_audio;
        true
    }

    /// Join a waiting room as player 2. The game starts the
    /// moment player 2 joins. Returns the player-2 token.
    pub fn join_room(&mut self, code: &str) -> Option<String> {
        // Generate the token first: holding `room` mutably while
        // calling `self.new_token()` would be a double mutable borrow.
        let token = self.new_token();
        let room = self.rooms.get_mut(code)?;
        if room.players.len() != 1 || room.started_at.is_some() {
            return None;
        }
        room.players.push(Player {
            token: token.clone(),
            responses: HashMap::new(),
        });
        room.started_at = Some(Instant::now());
        Some(token)
    }

    fn player_index(room: &Room, token: &str) -> Option<usize> {
        room.players.iter().position(|p| p.token == token)
    }

    /// Record a response for the trial currently in progress,
    /// judged by its timestamp (v0.2.2). `pressed_at_ms` is
    /// the ms-since-game-start when the local instance
    /// received the press (localhost, ~1ms after the click).
    /// Returns false for a wrong token, a waiting game, a
    /// trial whose timestamp does not check out, or an
    /// unknown room.
    pub fn record_response(
        &mut self,
        code: &str,
        token: &str,
        trial: usize,
        pressed_pos: bool,
        pressed_audio: bool,
        pressed_at_ms: u64,
    ) -> bool {
        let Some(room) = self.rooms.get_mut(code) else {
            return false;
        };
        if room.status() == "waiting" {
            return false;
        }
        if trial > room.completed_trials() {
            return false;
        }
        let arrival_ms = room
            .started_at
            .map(|s| s.elapsed().as_millis() as u64)
            .unwrap_or(0);
        if !valid_press(trial, pressed_at_ms, arrival_ms) {
            return false;
        }
        let Some(idx) = Self::player_index(room, token) else {
            return false;
        };
        let entry = room.players[idx]
            .responses
            .entry(trial)
            .or_insert((false, false));
        entry.0 |= pressed_pos;
        entry.1 |= pressed_audio;
        true
    }

    /// Full state JSON for one player. Never reveals future
    /// stimuli, the opponent's presses, or the other token.
    pub fn state_json(&self, code: &str, token: &str) -> Option<String> {
        let room = self.rooms.get(code)?;
        let you = Self::player_index(room, token)?;
        let status = room.status();
        let trial: i64 = match status {
            "playing" => room.current_trial().unwrap_or(0) as i64,
            "finished" => TOTAL_TRIALS as i64,
            _ => -1,
        };
        let (pos_json, letter_json) = match room.current_trial() {
            Some(t) => {
                let s = room.sequence[t];
                (s.pos.to_string(), format!("\"{}\"", s.letter))
            }
            None => ("null".to_string(), "null".to_string()),
        };
        let (your_score, your_hits, your_fa, your_misses) = player_stats(room, &room.players[you]);
        let (opp_score, opp_joined) = if room.players.len() > 1 {
            let opp = 1 - you;
            (player_stats(room, &room.players[opp]).0, true)
        } else {
            (0, false)
        };
        // Solo rooms have no opponent, so there is no winner
        // or loser — the final score is the result. Without
        // this check a negative solo score would wrongly read
        // as "opponent wins" (opponent_score is 0 by default).
        let solo = room.players.len() == 1;
        let winner_json = if solo || status != "finished" {
            "null".to_string()
        } else if your_score > opp_score {
            "1".to_string()
        } else if your_score < opp_score {
            "2".to_string()
        } else {
            "0".to_string()
        };
        // Winner is already relative to "you" (1 = you won,
        // 2 = opponent won, 0 = tie), because it was computed
        // from your_score vs opp_score above. No swap: swapping
        // for player 2 would invert the result for that player.
        Some(format!(
            "{{\"status\":\"{status}\",\"code\":\"{code}\",\"solo\":{solo},\"n\":{},\"total_trials\":{},\"trial_ms\":{},\"trial\":{trial},\"trial_remaining_ms\":{},\"pos\":{pos_json},\"letter\":{letter_json},\"you\":{},\"your_score\":{your_score},\"your_hits\":{your_hits},\"your_false_alarms\":{your_fa},\"your_misses\":{your_misses},\"opponent_score\":{opp_score},\"opponent_joined\":{opp_joined},\"peer_connected\":{},\"winner\":{winner_json}}}",
            room.n,
            TOTAL_TRIALS,
            TRIAL_MS,
            room.trial_remaining_ms(),
            you + 1,
            room.peer_connected,
        ))
    }
}

// --- Minimal JSON helpers for our fixed request shapes. ---

fn field_start<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\"");
    let idx = body.find(&pat)?;
    let rest = &body[idx + pat.len()..];
    let colon = rest.find(':')?;
    Some(&rest[colon + 1..])
}

/// `"key": "value"` -> Some(value)
pub fn parse_string_field(body: &str, key: &str) -> Option<String> {
    let rest = field_start(body, key)?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// `"key": true` / `"key": false` -> Some(bool)
pub fn parse_bool_field(body: &str, key: &str) -> Option<bool> {
    let rest = field_start(body, key)?.trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// `"key": 12` -> Some(12)
pub fn parse_usize_field(body: &str, key: &str) -> Option<usize> {
    let rest = field_start(body, key)?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// `"key": 123456789` -> Some(123456789) (u64, for seeds)
pub fn parse_u64_field(body: &str, key: &str) -> Option<u64> {
    let rest = field_start(body, key)?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// `room=ABCD&token=xyz` -> value for `key`.
pub fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(v);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_then_join_starts_game() {
        let mut state = ServerState::new();
        let (code, token1) = state.create_room(2);
        assert_eq!(state.rooms[&code].status(), "waiting");
        let token2 = state.join_room(&code).expect("join should succeed");
        assert_ne!(token1, token2);
        assert_eq!(state.rooms[&code].status(), "playing");
        // A third player cannot join.
        assert!(state.join_room(&code).is_none());
        // Unknown room cannot be joined.
        assert!(state.join_room("ZZZZ").is_none());
    }

    #[test]
    fn response_only_counts_for_current_trial() {
        let mut state = ServerState::new();
        let (code, token1) = state.create_room(2);
        // Not started yet: no response counts.
        assert!(!state.record_response(&code, &token1, 0, true, false, 100));
        let token2 = state.join_room(&code).unwrap();
        // Current trial is 0 right after start.
        assert!(state.record_response(&code, &token1, 0, true, false, 100));
        assert!(!state.record_response(&code, &token1, 5, true, false, 15_100));
        // Wrong token is rejected.
        assert!(!state.record_response(&code, "nope", 0, true, false, 100));
        // Player 2 can respond too.
        assert!(state.record_response(&code, &token2, 0, false, true, 100));
        // State JSON is visible to both players.
        assert!(state.state_json(&code, &token1).is_some());
        assert!(state.state_json(&code, &token2).is_some());
        assert!(state.state_json(&code, "nope").is_none());
    }

    #[test]
    fn p2p_room_shares_seed_and_records_peer_inputs() {
        // Both instances build from the same seed: identical
        // sequences, so both can score both players locally.
        let mut a = ServerState::new();
        let mut b = ServerState::new();
        let (code_a, token_a, _) = a.create_p2p_room(2, 4242);
        let (code_b, token_b, _) = b.create_p2p_room(2, 4242);
        assert_eq!(a.rooms[&code_a].sequence, b.rooms[&code_b].sequence);
        assert!(a.start_room(&code_a));
        assert!(b.start_room(&code_b));
        // Your press counts for the current trial.
        assert!(a.record_response(&code_a, &token_a, 0, true, false, 100));
        // A peer press for trial 0 counts on the other side,
        // and a future trial does not.
        assert!(b.record_peer_response(&code_b, 0, true, false, 100));
        assert!(!b.record_peer_response(&code_b, 5, true, false, 15_100));
        // Peer link flag is visible in state JSON.
        a.set_peer_connected(&code_a, true);
        let json = a.state_json(&code_a, &token_a).unwrap();
        assert!(json.contains("\"peer_connected\":true"));
        let _ = token_b;
    }

    #[test]
    fn solo_room_starts_immediately_with_no_winner() {
        let mut state = ServerState::new();
        let (code, token) = state.create_solo_room(2, 4242);
        assert_eq!(code, "SOLO");
        // One player, clock already running.
        assert_eq!(state.rooms[&code].status(), "playing");
        assert_eq!(state.rooms[&code].players.len(), 1);
        assert!(state.record_response(&code, &token, 0, true, false, 100));
        let json = state.state_json(&code, &token).unwrap();
        assert!(json.contains("\"solo\":true"));
        assert!(json.contains("\"opponent_joined\":false"));
        // No winner even when finished: a solo score is just a score.
        // (Simulate finished by checking the winner field is null
        // in a fresh solo room is not possible without waiting,
        // so assert the solo flag drives it via a finished room
        // built the same way in the P2P test's spirit: here we
        // only assert the flag is present and correct.)
        assert!(json.contains("\"winner\":null"));
    }

    #[test]
    fn timestamp_validation_table() {
        // On time, inside the trial: valid.
        assert!(valid_press(0, 100, 110));
        assert!(valid_press(1, 3_100, 3_120));
        // Press made on time but delivered late (arrival up
        // to 1s after the press): still valid — this is the
        // whole point of timestamping.
        assert!(valid_press(0, 100, 1_050));
        // Arrived more than 1s after the press: too stale.
        assert!(!valid_press(0, 100, 1_101));
        // Timestamp does not belong to the claimed trial.
        assert!(!valid_press(1, 100, 110));
        assert!(!valid_press(2, 100, 110));
        assert!(!valid_press(0, 6_100, 6_120));
        // Boundary grace: press made 50ms into trial 1 but
        // tagged trial 0 (browser poll lag) is accepted as
        // trial 0; 300ms in is too far.
        assert!(valid_press(0, 3_050, 3_060));
        assert!(!valid_press(0, 3_300, 3_310));
        // Timestamp claims a time far in the receiver's
        // future: clock skew or fabrication.
        assert!(!valid_press(0, 1_000, 100));
        // Past the end of the game.
        assert!(!valid_press(19, 60_000, 60_010));
        assert!(!valid_press(20, 60_000, 60_010));
    }

    #[test]
    fn json_field_parsing() {
        let body = r#"{"room": "ABCD", "trial": 12, "position": true, "audio": false}"#;
        assert_eq!(parse_string_field(body, "room").as_deref(), Some("ABCD"));
        assert_eq!(parse_usize_field(body, "trial"), Some(12));
        assert_eq!(parse_bool_field(body, "position"), Some(true));
        assert_eq!(parse_bool_field(body, "audio"), Some(false));
        assert_eq!(query_param("room=ABCD&token=xyz", "token"), Some("xyz"));
    }
}
