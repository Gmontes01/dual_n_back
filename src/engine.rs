//! Pure dual n-back engine. No I/O, no networking, no time.
//!
//! The server owns the sequence; both players see the same one.
//! Everything here is deterministic given a seed, which is what
//! makes a 1v1 fair: the server generates the sequence once per
//! room and scores both players against the same history.

use std::collections::VecDeque;

/// Letters used for the audio channel. Consonants only, so no
/// vowel sounds get confused with each other when spoken.
pub const LETTERS: [char; 8] = ['B', 'C', 'D', 'F', 'G', 'H', 'J', 'K'];

/// One trial: a grid position (0..8, row-major on a 3x3 grid)
/// plus a letter, presented at the same time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stimulus {
    pub pos: u8,
    pub letter: char,
}

/// Tiny deterministic RNG (xorshift64*). std has no seeded RNG,
/// and this crate has no dependencies, so we roll our own.
/// Same seed -> same sequence, every time, on every machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        // Never allow a zero state: it would stick at zero.
        let state = if seed == 0 { 0x9e3779b97f4a7c15 } else { seed };
        Self { state }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545f4914f6cdd1d)
    }

    /// Uniform value in 0..n.
    pub fn next_below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

/// Streaming dual n-back generator that produces stimuli on the fly
/// while keeping only a sliding window of the last `n` stimuli.
///
/// Because `Rng` is deterministic, two peers initialized with the same
/// `seed` and `n` will independently generate the exact same unbounded
/// stream of stimuli without ever storing the full sequence in memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SequenceStream {
    rng: Rng,
    n: usize,
    /// Sliding window of at most `n` recent stimuli (oldest at the front,
    /// newest at the back). The front element is the stimulus from `n`
    /// trials ago.
    history: VecDeque<Stimulus>,
    /// Most recently generated stimulus.
    current: Option<Stimulus>,
    /// Whether `current` matched the stimulus from `n` trials ago on
    /// (position, audio). Always `(false, false)` for the first `n` trials.
    current_matches: (bool, bool),
    /// Number of stimuli generated so far.
    trials_generated: usize,
}

impl SequenceStream {
    pub fn new(seed: u64, n: usize) -> Self {
        Self {
            rng: Rng::new(seed ^ 0x2545f4914f6cdd1d),
            n,
            history: VecDeque::with_capacity(n),
            current: None,
            current_matches: (false, false),
            trials_generated: 0,
        }
    }

    /// Advance the stream by one trial, generating the next `Stimulus`
    /// and returning `(stimulus, (pos_match, audio_match))`.
    ///
    /// If the history buffer has reached `n` items, the oldest stimulus
    /// (`n` trials back) is popped, used to bias and evaluate matches for
    /// the new stimulus, and replaced at the back by the new stimulus.
    pub fn next_trial(&mut self) -> (Stimulus, (bool, bool)) {
        let n_back = if self.n > 0 && self.history.len() == self.n {
            self.history.pop_front()
        } else {
            None
        };

        let (pos, letter) = match n_back {
            Some(prev) => {
                let pos = if self.rng.next_below(100) < 30 {
                    prev.pos
                } else {
                    self.rng.next_below(9) as u8
                };
                let letter = if self.rng.next_below(100) < 30 {
                    prev.letter
                } else {
                    LETTERS[self.rng.next_below(8) as usize]
                };
                (pos, letter)
            }
            None => {
                let pos = self.rng.next_below(9) as u8;
                let letter = LETTERS[self.rng.next_below(8) as usize];
                (pos, letter)
            }
        };

        let stim = Stimulus { pos, letter };
        let matched = match n_back {
            Some(prev) => (stim.pos == prev.pos, stim.letter == prev.letter),
            None => (false, false),
        };

        if self.n > 0 {
            self.history.push_back(stim);
        }

        self.current = Some(stim);
        self.current_matches = matched;
        self.trials_generated += 1;

        (stim, matched)
    }

    /// Generate and return only the next `Stimulus`.
    pub fn next_stimulus(&mut self) -> Stimulus {
        self.next_trial().0
    }

    /// The most recently generated stimulus, if `next_trial` has been called.
    pub fn current(&self) -> Option<Stimulus> {
        self.current
    }

    /// Whether the most recently generated stimulus matched `n` trials ago
    /// on `(position, audio)`.
    pub fn current_matches(&self) -> (bool, bool) {
        self.current_matches
    }

    /// View the sliding window of the last `<= n` stimuli (oldest to newest).
    pub fn history(&self) -> &VecDeque<Stimulus> {
        &self.history
    }

    /// Total number of stimuli generated so far.
    pub fn trials_generated(&self) -> usize {
        self.trials_generated
    }

    /// Configured `n`-back distance.
    pub fn n(&self) -> usize {
        self.n
    }
}

/// Generate a fixed-length sequence up front (kept for testing and
/// compatibility). Uses `SequenceStream` under the hood so batch and
/// streaming generation are guaranteed to be identical.
///
/// With purely random stimuli, a position match would happen
/// about 1/9 of the time and a letter match about 1/8. That is
/// too rare to be fun, so with ~30% probability each channel
/// copies the stimulus from `n` trials ago, independently.
/// That lands the match rate near 30% per channel.
pub fn generate_sequence(seed: u64, n: usize, total: usize) -> Vec<Stimulus> {
    let mut stream = SequenceStream::new(seed, n);
    let mut seq = Vec::with_capacity(total);
    for _ in 0..total {
        seq.push(stream.next_stimulus());
    }
    seq
}

/// For trial `trial`, was there a position match and/or a
/// letter match against the trial `n` back? The first `n`
/// trials have nothing to match against, so both are false.
pub fn matches(seq: &[Stimulus], trial: usize, n: usize) -> (bool, bool) {
    if trial < n || trial >= seq.len() {
        return (false, false);
    }
    let cur = seq[trial];
    let back = seq[trial - n];
    (cur.pos == back.pos, cur.letter == back.letter)
}

/// Score one channel: hit +10, false alarm -5, miss and
/// correct rejection 0. Misses are not punished here on
/// purpose -- in a 1v1, spamming both buttons every trial
/// should lose, and -5 per false alarm does that, while
/// a cautious player who misses a match just scores 0.
pub fn score_one(expected: bool, pressed: bool) -> i32 {
    match (expected, pressed) {
        (true, true) => 10, //score 10 for correct label
        (false, true) => -5, //lose 5 for false positive
        (true, false) => -5, //lose 5 for missed label
        _ => 0,
    }
}

/// Score one trial across both channels. Range: -10..=20.
pub fn score_trial(
    expected_pos: bool,
    expected_audio: bool,
    pressed_pos: bool,
    pressed_audio: bool,
) -> i32 {
    score_one(expected_pos, pressed_pos) + score_one(expected_audio, pressed_audio)
}

/// Tracks a player's cumulative score, hits, false alarms, and misses
/// directly in the engine so the server does not duplicate scoring rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlayerStats {
    pub score: i32,
    pub hits: u32,
    pub false_alarms: u32,
    pub misses: u32,
}

impl PlayerStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Score one channel (position or audio) and update hit, false-alarm,
    /// and miss counts alongside `score`.
    pub fn record_channel(&mut self, expected: bool, pressed: bool) {
        self.score += score_one(expected, pressed);
        match (expected, pressed) {
            (true, true) => self.hits += 1,
            (false, true) => self.false_alarms += 1,
            (true, false) => self.misses += 1,
            (false, false) => {}
        }
    }

    /// Score both channels for one completed trial.
    pub fn record_trial(
        &mut self,
        expected_pos: bool,
        expected_audio: bool,
        pressed_pos: bool,
        pressed_audio: bool,
    ) {
        self.record_channel(expected_pos, pressed_pos);
        self.record_channel(expected_audio, pressed_audio);
    }

    /// Adjust stats when a valid late press arrives (within the grace /
    /// latency window) for a trial channel that was already scored as unpressed.
    pub fn apply_late_press(&mut self, expected: bool, newly_pressed: bool) {
        if !newly_pressed {
            return;
        }
        self.score -= score_one(expected, false);
        if expected {
            self.misses = self.misses.saturating_sub(1);
        }
        self.record_channel(expected, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let a = generate_sequence(42, 2, 20);
        let b = generate_sequence(42, 2, 20);
        assert_eq!(a, b);
    }

    #[test]
    fn different_seed_different_sequence() {
        let a = generate_sequence(42, 2, 20);
        let b = generate_sequence(43, 2, 20);
        assert_ne!(a, b);
    }

    #[test]
    fn sequence_values_are_in_range() {
        let seq = generate_sequence(7, 2, 50);
        assert_eq!(seq.len(), 50);
        for s in &seq {
            assert!(s.pos < 9);
            assert!(LETTERS.contains(&s.letter));
        }
    }

    #[test]
    fn first_n_trials_never_match() {
        let seq = generate_sequence(99, 2, 20);
        assert_eq!(matches(&seq, 0, 2), (false, false));
        assert_eq!(matches(&seq, 1, 2), (false, false));
    }

    #[test]
    fn matches_agrees_with_manual_comparison() {
        // Hand-built sequence: trial 2 repeats trial 0 exactly.
        let seq = vec![
            Stimulus { pos: 0, letter: 'B' },
            Stimulus { pos: 1, letter: 'C' },
            Stimulus { pos: 0, letter: 'B' },
            Stimulus { pos: 1, letter: 'D' },
        ];
        assert_eq!(matches(&seq, 2, 2), (true, true));
        // Trial 3: position matches trial 1, letter does not.
        assert_eq!(matches(&seq, 3, 2), (true, false));
    }

    #[test]
    fn scoring_table() {
        assert_eq!(score_one(true, true), 10); // hit
        assert_eq!(score_one(false, true), -5); // false alarm
        assert_eq!(score_one(true, false), -5); // miss
        assert_eq!(score_one(false, false), 0); // correct rejection
        assert_eq!(score_trial(true, false, true, true), 10 - 5);
        assert_eq!(score_trial(true, true, true, true), 20);
        assert_eq!(score_trial(false, false, true, true), -10);
    }

    #[test]
    fn match_rate_is_not_tiny() {
        // With the 30% copy bias, a 200-trial sequence should
        // have a healthy number of matches, not ~1/9.
        let seq = generate_sequence(1234, 2, 200);
        let pos_matches = (2..200).filter(|&t| matches(&seq, t, 2).0).count();
        assert!(pos_matches > 30, "pos matches: {pos_matches}");
        assert!(pos_matches < 90, "pos matches: {pos_matches}");
    }

    #[test]
    fn stream_matches_batch_sequence_and_bounds_history() {
        let seed = 4242;
        let n = 3;
        let total = 100;
        let expected_seq = generate_sequence(seed, n, total);
        let mut stream = SequenceStream::new(seed, n);

        for (t, &expected_stim) in expected_seq.iter().enumerate() {
            let (stim, stream_match) = stream.next_trial();
            assert_eq!(stim, expected_stim);
            assert_eq!(stream.current(), Some(expected_stim));
            assert_eq!(stream_match, matches(&expected_seq, t, n));
            assert_eq!(stream.current_matches(), stream_match);
            assert_eq!(stream.trials_generated(), t + 1);
            // Sliding window never holds more than `n` stimuli.
            assert!(stream.history().len() <= n);
            assert_eq!(stream.history().len(), (t + 1).min(n));
        }
    }

    #[test]
    fn two_peers_with_same_seed_stay_in_lockstep() {
        let mut host_stream = SequenceStream::new(987654321, 2);
        let mut joiner_stream = SequenceStream::new(987654321, 2);

        for _ in 0..500 {
            assert_eq!(host_stream.next_trial(), joiner_stream.next_trial());
        }
    }
}
