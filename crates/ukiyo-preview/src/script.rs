//! Scripted signal timelines, for recording demos and checking reactions.
//!
//! ```toml
//! duration = 40          # seconds
//! fps = 30
//! size = [1920, 1080]
//! hour = 22              # starting time of day (omit: the real clock)
//!
//! [[at]]
//! t = 4
//! caption = "Cursor: parallax"
//! ramp = 1.5             # seconds to glide from the previous values
//! set = { "cursor.x" = 0.8, "cursor.y" = -0.3 }
//!
//! [[at]]
//! t = 12
//! pulse = ["build_failed"]
//!
//! [[at]]
//! t = 20
//! hour = 3               # time of day, ramped like `set`
//! ramp = 3
//!
//! [[at]]
//! t = 30
//! still = true           # the lock
//! ```

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    pub duration: f64,
    #[serde(default = "default_fps")]
    pub fps: f64,
    #[serde(default)]
    pub size: Option<[u32; 2]>,
    #[serde(default)]
    pub hour: Option<f32>,
    #[serde(default)]
    pub at: Vec<Event>,
}

fn default_fps() -> f64 {
    30.0
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub t: f64,
    #[serde(default)]
    pub ramp: f64,
    #[serde(default)]
    pub set: BTreeMap<String, f32>,
    #[serde(default)]
    pub pulse: Vec<String>,
    #[serde(default)]
    pub hour: Option<f32>,
    #[serde(default)]
    pub still: Option<bool>,
    #[serde(default)]
    pub caption: Option<String>,
    /// Fake music at this tempo (beats per minute); 0 stops it.
    #[serde(default)]
    pub music: Option<f32>,
}

/// What the script says at a moment.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct State {
    pub values: BTreeMap<String, f32>,
    pub hour: Option<f32>,
    pub still: bool,
    pub caption: Option<String>,
    /// Seconds since the caption appeared (for fades).
    pub caption_age: f64,
    /// Tempo of the fake music and when it started.
    pub music: Option<(f32, f64)>,
}

impl Script {
    pub fn parse(text: &str) -> Result<Script, String> {
        let mut s: Script = toml::from_str(text).map_err(|e| e.to_string())?;
        if !(s.duration > 0.0 && s.fps > 0.0) {
            return Err("duration and fps must be positive".into());
        }
        s.at.sort_by(|a, b| a.t.total_cmp(&b.t));
        Ok(s)
    }

    /// The state at time `t`: every `set` and `hour` ramps linearly (with
    /// smoothstep easing) from the value before its event over `ramp`.
    pub fn state(&self, t: f64) -> State {
        let mut st = State {
            hour: self.hour,
            ..State::default()
        };
        for e in self.at.iter().filter(|e| e.t <= t) {
            let k = if e.ramp > 0.0 {
                ease(((t - e.t) / e.ramp).clamp(0.0, 1.0))
            } else {
                1.0
            };
            for (name, v) in &e.set {
                let from = st.values.get(name).copied().unwrap_or(0.0);
                st.values.insert(name.clone(), from + (v - from) * k as f32);
            }
            if let Some(h) = e.hour {
                st.hour = Some(match st.hour {
                    Some(from) => {
                        // The short way round the clock.
                        let mut d = (h - from).rem_euclid(24.0);
                        if d > 12.0 {
                            d -= 24.0;
                        }
                        (from + d * k as f32).rem_euclid(24.0)
                    }
                    None => h,
                });
            }
            if let Some(s) = e.still {
                st.still = s;
            }
            if let Some(bpm) = e.music {
                st.music = (bpm > 0.0).then_some((bpm, e.t));
            }
            if let Some(c) = &e.caption {
                st.caption = (!c.is_empty()).then(|| c.clone());
                st.caption_age = t - e.t;
            }
        }
        st
    }

    /// Pulses that fire in (t0, t1]; at t0 = None, those at exactly the start too.
    pub fn pulses(&self, t0: Option<f64>, t1: f64) -> Vec<String> {
        self.at
            .iter()
            .filter(|e| t0.map_or(e.t <= t1, |t0| e.t > t0 && e.t <= t1))
            .flat_map(|e| e.pulse.iter().cloned())
            .collect()
    }
}

/// Synthetic audio signals for a four-on-the-floor beat at `bpm`, `t`
/// seconds in: (level, bass, mid, treble, beat).
pub fn fake_music(bpm: f32, t: f64) -> (f32, f32, f32, f32, f32) {
    let beat_len = 60.0 / f64::from(bpm.max(1.0));
    let phase = (t / beat_len).fract();
    let since = (phase * beat_len) as f32;
    let bass = (-since / 0.11).exp();
    let eighth = ((t / (beat_len / 2.0)).fract() * beat_len / 2.0) as f32;
    let treble = 0.25 + 0.5 * (-eighth / 0.05).exp();
    let bar = (t / (beat_len * 4.0)).fract() as f32;
    let mid = 0.35 + 0.25 * (bar * std::f32::consts::TAU).sin().abs();
    let beat = if since < 0.04 { 1.0 } else { 0.0 };
    (0.55 + 0.3 * bass, bass, mid, treble, beat)
}

fn ease(x: f64) -> f64 {
    x * x * (3.0 - 2.0 * x)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = r#"
duration = 10
hour = 22
[[at]]
t = 0
caption = "Idle"
set = { "cpu.load" = 0.1 }
[[at]]
t = 2
ramp = 2
set = { "cpu.load" = 0.9 }
caption = "Build"
[[at]]
t = 5
pulse = ["build_failed"]
[[at]]
t = 6
hour = 2
ramp = 2
[[at]]
t = 9
still = true
"#;

    #[test]
    fn ramps_and_holds() {
        let s = Script::parse(S).unwrap();
        assert_eq!(s.state(1.0).values["cpu.load"], 0.1);
        assert!((s.state(3.0).values["cpu.load"] - 0.5).abs() < 1e-6);
        assert_eq!(s.state(4.5).values["cpu.load"], 0.9);
        assert_eq!(s.state(1.0).caption.as_deref(), Some("Idle"));
        assert_eq!(s.state(2.5).caption.as_deref(), Some("Build"));
        assert!((s.state(2.5).caption_age - 0.5).abs() < 1e-9);
    }

    #[test]
    fn hour_goes_the_short_way() {
        let s = Script::parse(S).unwrap();
        assert_eq!(s.state(5.0).hour, Some(22.0));
        assert_eq!(s.state(7.0).hour, Some(0.0), "22 -> 2 through midnight");
        assert_eq!(s.state(8.0).hour, Some(2.0));
    }

    #[test]
    fn pulses_and_still() {
        let s = Script::parse(S).unwrap();
        assert_eq!(s.pulses(Some(4.9), 5.0), vec!["build_failed".to_string()]);
        assert!(s.pulses(Some(5.0), 5.1).is_empty());
        assert!(s.pulses(None, 4.0).is_empty());
        assert!(!s.state(8.9).still);
        assert!(s.state(9.0).still);
    }

    #[test]
    fn music_beats_on_time() {
        let (_, bass0, _, _, beat0) = fake_music(120.0, 0.0);
        assert_eq!((bass0, beat0), (1.0, 1.0));
        let (_, bass, _, _, beat) = fake_music(120.0, 0.25);
        assert!(bass < 0.15 && beat == 0.0);
        let (_, _, _, _, beat) = fake_music(120.0, 0.51);
        assert_eq!(beat, 1.0);
    }

    #[test]
    fn rejects_bad_scripts() {
        assert!(Script::parse("duration = 0").is_err());
        assert!(Script::parse("duration = 1\n[[at]]\nt = 0\nbogus = 1\n").is_err());
    }
}
