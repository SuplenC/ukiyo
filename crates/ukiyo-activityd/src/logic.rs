//! The pure part of the helper: counting, the sliding window, quantising,
//! who may connect. See `docs/SECURITY.md` for why it looks like this.

/// Publishing cadence, milliseconds. Fixed: one line per tick whether or
/// not anything changed, so message timing says nothing about keystrokes.
pub const TICK_MS: u64 = 250;
/// Sliding window, in ticks (2 s).
pub const WINDOW_TICKS: usize = 8;
/// Rates are rounded to multiples of this (keys/second).
pub const STEP: f32 = 0.5;

pub const EV_KEY: u16 = 1;
/// Size of `struct input_event` on 64-bit Linux (timeval, type, code, value).
pub const EVENT_SIZE: usize = 24;

/// Key presses (value 1) in a buffer of `input_event`s. Repeats (2) and
/// releases (0) don't count; the key code is never looked at.
pub fn count_presses(buf: &[u8]) -> u32 {
    buf.as_chunks::<EVENT_SIZE>()
        .0
        .iter()
        .filter(|e| {
            let kind = u16::from_ne_bytes([e[16], e[17]]);
            let value = i32::from_ne_bytes([e[20], e[21], e[22], e[23]]);
            kind == EV_KEY && value == 1
        })
        .count() as u32
}

/// Does a device's EV_KEY capability bitmap have letter keys (a keyboard,
/// not a power button or a mouse)?
pub fn is_keyboard(key_bits: &[u8]) -> bool {
    let has = |code: usize| {
        key_bits
            .get(code / 8)
            .is_some_and(|b| b & (1 << (code % 8)) != 0)
    };
    // KEY_Q..KEY_P, KEY_A..KEY_L, KEY_Z..KEY_M.
    (16..=25).chain(30..=38).chain(44..=50).all(has)
}

/// Round to the nearest [`STEP`].
pub fn quantise(rate: f32) -> f32 {
    if !rate.is_finite() || rate <= 0.0 {
        return 0.0;
    }
    (rate / STEP).round() * STEP
}

/// Per-tick press counts over the window. Only counts are kept: no
/// timestamps, no codes.
#[derive(Debug, Default)]
pub struct Window {
    buckets: [u32; WINDOW_TICKS],
    next: usize,
}

impl Window {
    /// Close a tick with `presses` and return the published rate
    /// (keys/second, quantised).
    pub fn tick(&mut self, presses: u32) -> f32 {
        self.buckets[self.next] = presses;
        self.next = (self.next + 1) % WINDOW_TICKS;
        let total: u32 = self.buckets.iter().sum();
        let secs = (WINDOW_TICKS as u64 * TICK_MS) as f32 / 1000.0;
        quantise(total as f32 / secs)
    }
}

/// `ACTIVE_UID=` from `/run/systemd/seats/seat0` (None: no active session).
pub fn parse_active_uid(text: &str) -> Option<u32> {
    text.lines()
        .find_map(|l| l.strip_prefix("ACTIVE_UID="))
        .and_then(|v| v.trim().parse().ok())
}

/// Who may connect: root, or the user of the active session on seat0.
pub fn allowed(peer_uid: u32, active_uid: Option<u32>) -> bool {
    peer_uid == 0 || Some(peer_uid) == active_uid
}

/// What a client gets: the rate while its session is the active one (root
/// always), 0 otherwise.
pub fn visible_rate(peer_uid: u32, active_uid: Option<u32>, rate: f32) -> f32 {
    if allowed(peer_uid, active_uid) {
        rate
    } else {
        0.0
    }
}

pub fn line(rate: f32) -> String {
    format!("{{\"rate\":{rate:.1}}}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: u16, code: u16, value: i32) -> Vec<u8> {
        let mut e = vec![0u8; 16];
        e.extend_from_slice(&kind.to_ne_bytes());
        e.extend_from_slice(&code.to_ne_bytes());
        e.extend_from_slice(&value.to_ne_bytes());
        e
    }

    #[test]
    fn counts_presses_only() {
        let mut buf = Vec::new();
        buf.extend(event(EV_KEY, 30, 1)); // press
        buf.extend(event(0, 0, 0)); // SYN
        buf.extend(event(EV_KEY, 30, 2)); // repeat
        buf.extend(event(EV_KEY, 30, 0)); // release
        buf.extend(event(EV_KEY, 31, 1)); // press
        buf.extend(event(4, 4, 1)); // MSC_SCAN with value 1: not a key
        buf.extend([0u8; 7]); // a torn tail is ignored
        assert_eq!(count_presses(&buf), 2);
    }

    #[test]
    fn keyboards_have_letters() {
        let mut bits = [0u8; 96];
        assert!(!is_keyboard(&bits));
        for c in (16..=25).chain(30..=38).chain(44..=50) {
            bits[c / 8] |= 1 << (c % 8);
        }
        assert!(is_keyboard(&bits));
        bits[44 / 8] &= !(1 << (44 % 8));
        assert!(!is_keyboard(&bits), "no Z: not a full keyboard");
        assert!(!is_keyboard(&[]));
    }

    #[test]
    fn quantised_to_half_steps() {
        assert_eq!(quantise(0.0), 0.0);
        assert_eq!(quantise(0.24), 0.0);
        assert_eq!(quantise(0.26), 0.5);
        assert_eq!(quantise(3.7), 3.5);
        assert_eq!(quantise(3.76), 4.0);
        assert_eq!(quantise(f32::NAN), 0.0);
        assert_eq!(quantise(-1.0), 0.0);
    }

    #[test]
    fn sliding_window() {
        let mut w = Window::default();
        // 2 presses per 250 ms tick = 8 keys/s once the window is full.
        let rates: Vec<f32> = (0..10).map(|_| w.tick(2)).collect();
        assert_eq!(rates[0], 1.0);
        assert_eq!(rates[7], 8.0);
        assert_eq!(rates[9], 8.0);
        // A single key in 2 s: 0.5 keys/s, then gone after the window.
        let mut w = Window::default();
        assert_eq!(w.tick(1), 0.5);
        for _ in 0..7 {
            assert_eq!(w.tick(0), 0.5);
        }
        assert_eq!(w.tick(0), 0.0);
    }

    #[test]
    fn seat_file() {
        let text = "# This is private data. Do not parse.\nIS_SEAT0=1\nCAN_MULTI_SESSION=1\nCAN_TTY=1\nCAN_GRAPHICAL=1\nACTIVE=2\nACTIVE_UID=1000\nSESSIONS=2 c1\nUIDS=1000 120\n";
        assert_eq!(parse_active_uid(text), Some(1000));
        assert_eq!(parse_active_uid("IS_SEAT0=1\nCAN_TTY=1\n"), None);
        assert_eq!(parse_active_uid("ACTIVE_UID=x\n"), None);
    }

    #[test]
    fn access() {
        assert!(allowed(0, None));
        assert!(allowed(1000, Some(1000)));
        assert!(!allowed(1001, Some(1000)));
        assert!(!allowed(1000, None));
        assert_eq!(visible_rate(1000, Some(1000), 3.5), 3.5);
        assert_eq!(visible_rate(1000, Some(1001), 3.5), 0.0, "switched away");
        assert_eq!(line(3.5), "{\"rate\":3.5}\n");
        assert_eq!(line(0.0), "{\"rate\":0.0}\n");
    }
}
