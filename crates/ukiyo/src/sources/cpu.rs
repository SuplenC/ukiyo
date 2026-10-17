//! `cpu.load`: the aggregate busy fraction from `/proc/stat`, averaged over
//! `signals.cpu.window`. The daemon samples a few times per window, and only
//! while it is drawing.

use std::collections::VecDeque;

/// Jiffies from the aggregate `cpu` line: (busy, total).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Times {
    pub busy: u64,
    pub total: u64,
}

/// Parse the first (`cpu `) line of `/proc/stat`.
pub fn parse_stat(text: &str) -> Option<Times> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    if v.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal [guest guest_nice]:
    // guest time is already counted in user/nice.
    let total: u64 = v.iter().take(8).sum();
    let idle = v[3] + v.get(4).copied().unwrap_or(0);
    Some(Times {
        busy: total.saturating_sub(idle),
        total,
    })
}

/// Busy fraction between two samples.
pub fn load(a: Times, b: Times) -> Option<f32> {
    let dt = b.total.checked_sub(a.total)?;
    if dt == 0 {
        return None;
    }
    let busy = b.busy.saturating_sub(a.busy).min(dt);
    Some(busy as f32 / dt as f32)
}

/// A sliding window of samples.
#[derive(Debug, Default)]
pub struct Sampler {
    samples: VecDeque<(f64, Times)>,
}

impl Sampler {
    /// Add a sample at `now` (seconds) and return the load over the last
    /// `window` seconds (or since the oldest sample we have).
    pub fn push(&mut self, now: f64, t: Times, window: f64) -> Option<f32> {
        // A sample from long ago (we were paused) says nothing about now.
        if self
            .samples
            .back()
            .is_some_and(|(at, _)| now - at > window * 2.0)
        {
            self.samples.clear();
        }
        self.samples.push_back((now, t));
        while self.samples.len() > 2 && now - self.samples[1].0 >= window {
            self.samples.pop_front();
        }
        let (_, first) = *self.samples.front()?;
        load(first, t)
    }

    pub fn read() -> Option<Times> {
        parse_stat(&std::fs::read_to_string("/proc/stat").ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_stat() {
        let text = "cpu  10132153 290696 3084719 46828483 16683 0 25195 0 175628 0\n\
                    cpu0 1393280 32966 572056 13343292 6130 0 17875 0 23933 0\n\
                    intr 1462898 0 0\n";
        let t = parse_stat(text).unwrap();
        let total = 10132153 + 290696 + 3084719 + 46828483 + 16683 + 25195;
        assert_eq!(t.total, total);
        assert_eq!(t.busy, total - 46828483 - 16683);
        assert!(parse_stat("intr 1 2\n").is_none());
        assert!(parse_stat("cpu  1 2\n").is_none());
    }

    #[test]
    fn delta_math() {
        let a = Times {
            busy: 100,
            total: 1000,
        };
        let b = Times {
            busy: 175,
            total: 1100,
        };
        assert_eq!(load(a, b), Some(0.75));
        assert_eq!(load(a, a), None, "no time passed");
        assert_eq!(load(b, a), None, "counter went backwards");
    }

    #[test]
    fn window_slides() {
        let mut s = Sampler::default();
        let t = |busy, total| Times { busy, total };
        assert_eq!(s.push(0.0, t(0, 0), 1.0), None);
        // 0..0.5 s fully busy, 0.5..1 s idle.
        assert_eq!(s.push(0.5, t(50, 50), 1.0), Some(1.0));
        assert_eq!(s.push(1.0, t(50, 100), 1.0), Some(0.5));
        // The first sample has left the window: only 0.5..1.5 counts.
        assert_eq!(s.push(1.5, t(50, 150), 1.0), Some(0.0));
        // After a long pause the old samples are dropped.
        assert_eq!(s.push(60.0, t(5000, 6000), 1.0), None);
        assert_eq!(s.push(60.25, t(5025, 6025), 1.0), Some(1.0));
    }
}
