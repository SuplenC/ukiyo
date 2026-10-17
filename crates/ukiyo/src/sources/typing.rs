//! `typing.rate`, 0..1 (about 8 keys a second reads 1). Never from key
//! codes; two sources:
//!
//! - **idle** (default, no privileges): ext-idle-notify-v1 with a short
//!   timeout. Between `resumed` and `idled` the user is giving input; input
//!   while the cursor stands still is, approximately, typing. The rate is
//!   the fraction of the last [`WINDOW`] seconds spent that way. It can't
//!   tell 3 from 8 keys a second within a burst, and where the cursor can't
//!   be seen (no Hyprland, cursor over windows) mouse use reads as typing
//!   too. Good enough for a wallpaper that reacts to "you're typing".
//! - **helper**: the aggregate rate from `ukiyo-activityd` (keys/second,
//!   quantised, coarse cadence), read from its socket.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Idle-notification timeout for the activity source, milliseconds.
pub const IDLE_TIMEOUT_MS: u32 = 400;
/// Averaging window, seconds.
pub const WINDOW: f64 = 2.0;
/// Input this soon after the cursor moved is the mouse, not typing.
const POINTER_GRACE: f64 = 0.5;
/// What one isolated keystroke counts as, seconds.
const MIN_SPAN: f64 = 0.125;
/// Keys per second that read as 1.
pub const FULL_RATE: f32 = 8.0;

#[derive(Debug, Default)]
pub struct IdleTyping {
    active_since: Option<f64>,
    spans: VecDeque<(f64, f64)>,
    last_pointer: Option<f64>,
}

impl IdleTyping {
    pub fn new() -> Self {
        Self::default()
    }

    /// Input after a quiet spell.
    pub fn resumed(&mut self, t: f64) {
        if self.active_since.is_none() {
            self.active_since = Some(t);
        }
    }

    /// No input for the idle timeout: the activity ended a timeout ago.
    pub fn idled(&mut self, t: f64) {
        if let Some(start) = self.active_since.take() {
            let end = (t - f64::from(IDLE_TIMEOUT_MS) / 1000.0).max(start);
            self.spans.push_back((start, end.max(start + MIN_SPAN)));
        }
    }

    pub fn pointer_moved(&mut self, t: f64) {
        self.last_pointer = Some(t);
    }

    /// The typing rate now, 0..1.
    pub fn rate(&mut self, now: f64) -> f32 {
        let from = now - WINDOW;
        while self.spans.front().is_some_and(|s| s.1 < from) {
            self.spans.pop_front();
        }
        // Nothing counts while (or just after) the mouse moves.
        let floor = self
            .last_pointer
            .map_or(from, |p| from.max(p + POINTER_GRACE));
        let open = self.active_since.map(|s| (s, now.max(s + MIN_SPAN)));
        let busy: f64 = self
            .spans
            .iter()
            .copied()
            .chain(open)
            .map(|(a, b)| (b.min(now) - a.max(floor)).max(0.0))
            .sum();
        (busy / WINDOW).clamp(0.0, 1.0) as f32
    }
}

/// A line from `ukiyo-activityd`: `{"rate":3.5}` → keys per second.
pub fn parse_helper_line(line: &str) -> Option<f32> {
    #[derive(serde::Deserialize)]
    struct Msg {
        rate: f32,
    }
    let m: Msg = serde_json::from_str(line.trim()).ok()?;
    m.rate.is_finite().then_some(m.rate.max(0.0))
}

/// keys/s → `typing.rate`.
pub fn helper_rate(keys_per_sec: f32) -> f32 {
    (keys_per_sec / FULL_RATE).clamp(0.0, 1.0)
}

/// Does the helper answer at `path`?
pub fn helper_available(path: &str) -> bool {
    !path.is_empty() && UnixStream::connect(path).is_ok()
}

/// A connection to the helper, read on a thread into `out` (None once it's
/// gone). Dropping it disconnects.
pub struct HelperClient {
    stream: UnixStream,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HelperClient {
    pub fn connect(path: &str, out: Arc<Mutex<Option<f32>>>) -> std::io::Result<HelperClient> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let reader = stream.try_clone()?;
        let thread = std::thread::Builder::new()
            .name("ukiyo-typing".into())
            .spawn(move || {
                let mut r = BufReader::new(reader);
                let mut line = String::new();
                loop {
                    line.clear();
                    match r.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if let Some(rate) = parse_helper_line(&line)
                                && let Ok(mut o) = out.lock()
                            {
                                *o = Some(rate);
                            }
                        }
                    }
                }
                if let Ok(mut o) = out.lock() {
                    *o = None;
                }
            })?;
        Ok(HelperClient {
            stream,
            thread: Some(thread),
        })
    }

    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(|t| t.is_finished())
    }
}

impl Drop for HelperClient {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_typing_reads_full() {
        let mut t = IdleTyping::new();
        t.resumed(0.0);
        assert!((t.rate(2.0) - 1.0).abs() < 1e-6);
        // Stops typing at 2.0: idled arrives 0.4 s later.
        t.idled(2.4);
        assert!((t.rate(2.4) - 0.8).abs() < 1e-3, "{}", t.rate(2.4));
        assert!(t.rate(3.0) < 0.6);
        assert_eq!(t.rate(4.1), 0.0);
    }

    #[test]
    fn bursts_average_out() {
        let mut t = IdleTyping::new();
        // Half a second of typing, then a pause, twice.
        for k in 0..2 {
            let s = f64::from(k);
            t.resumed(s);
            t.idled(s + 0.5 + 0.4);
        }
        assert!((t.rate(2.0) - 0.5).abs() < 1e-6, "{}", t.rate(2.0));
        // A single keystroke counts a little.
        let mut t = IdleTyping::new();
        t.resumed(10.0);
        t.idled(10.4);
        let r = t.rate(10.5);
        assert!(r > 0.0 && r < 0.1, "{r}");
    }

    #[test]
    fn mouse_is_not_typing() {
        let mut t = IdleTyping::new();
        t.resumed(0.0);
        t.pointer_moved(1.0);
        assert_eq!(t.rate(1.2), 0.0);
        // Typing again after the mouse stopped counts from then.
        assert!((t.rate(2.5) - 0.5).abs() < 1e-6, "{}", t.rate(2.5));
    }

    #[test]
    fn helper_lines() {
        assert_eq!(parse_helper_line("{\"rate\":3.5}\n"), Some(3.5));
        assert_eq!(parse_helper_line("{\"rate\":-1}"), Some(0.0));
        assert_eq!(parse_helper_line("garbage"), None);
        assert_eq!(helper_rate(4.0), 0.5);
        assert_eq!(helper_rate(20.0), 1.0);
    }

    #[test]
    fn helper_client_reads_until_gone() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("ukiyo-typing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s");
        let l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let out = Arc::new(Mutex::new(None));
        assert!(helper_available(path.to_str().unwrap()));
        let _ = l.accept(); // the probe
        let c = HelperClient::connect(path.to_str().unwrap(), out.clone()).unwrap();
        let (mut s, _) = l.accept().unwrap();
        s.write_all(b"{\"rate\":2.5}\n").unwrap();
        for _ in 0..100 {
            if out.lock().unwrap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(*out.lock().unwrap(), Some(2.5));
        drop(s);
        for _ in 0..100 {
            if c.finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(c.finished());
        assert_eq!(*out.lock().unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
