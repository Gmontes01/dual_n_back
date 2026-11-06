//! Pure dual n-back engine. No I/O, no networking, no time.
//!
//! The server owns the sequence; both players see the same one.
//! Everything here is deterministic given a seed, which is what
//! makes a 1v1 fair: the server generates the sequence once per
//! room and scores both players against the same history.

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

/// Generate the full sequence for a room up front.
///
/// With purely random stimuli, a position match would happen
/// about 1/9 of the time and a letter match about 1/8. That is
/// too rare to be fun, so with ~30% probability each channel
/// copies the stimulus from `n` trials ago, independently.
/// That lands the match rate near 30% per channel.
pub fn generate_sequence(seed: u64, n: usize, total: usize) -> Vec<Stimulus> {
    let mut rng = Rng::new(seed ^ 0x2545f4914f6cdd1d);
    let mut seq: Vec<Stimulus> = Vec::with_capacity(total);
    for i in 0..total {
        let mut pos = rng.next_below(9) as u8;
        let mut letter = LETTERS[rng.next_below(8) as usize];
        if i >= n {
            if rng.next_below(100) < 30 {
                pos = seq[i - n].pos;
            }
            if rng.next_below(100) < 30 {
                letter = seq[i - n].letter;
            }
        }
        seq.push(Stimulus { pos, letter });
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
        (true, true) => 10,
        (false, true) => -5,
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
        assert_eq!(score_one(true, false), 0); // miss
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
}
