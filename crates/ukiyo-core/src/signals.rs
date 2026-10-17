//! The signal model.
//!
//! Sources (the daemon's Hyprland, CPU, audio and typing readers, the
//! preview's sliders, `ukiyo signal`) write raw values into a [`SignalBus`]
//! by name. Once per frame [`SignalBus::update`] turns them into the values
//! scenes read: per group (`cpu` for `cpu.load`, ...) the user's on/off,
//! gain and attack/release smoothing apply, times the global intensity.
//! A signal nobody writes reads 0, so every source is optional.
//!
//! Pulses are events (`ukiyo pulse build_failed`): `pulse.<name>` jumps to 1
//! and decays (time constant [`PULSE_DECAY`]); `pulse.<name>.age` is the
//! seconds since it fired.

use std::collections::HashMap;

use crate::settings::Settings;
use crate::smooth::{Smoother, Smoothing};

/// Seconds for a pulse envelope to fall to 37%.
pub const PULSE_DECAY: f32 = 0.5;
/// What `pulse.<name>.age` reads before the pulse ever fired.
pub const NEVER: f32 = 1.0e6;

/// The signals ukiyo itself knows about (sources may add more).
pub const KNOWN: &[(&str, &str)] = &[
    (
        "cursor.x",
        "Cursor, -1 (left edge) .. 1 (right edge) of the output",
    ),
    ("cursor.y", "Cursor, -1 (top) .. 1 (bottom)"),
    (
        "workspace.pan",
        "Camera pan by workspace, -1 (first) .. 1 (ninth and on)",
    ),
    ("workspace.id", "Active workspace id (raw)"),
    ("typing.rate", "Typing, 0 .. 1 (about 8 keys a second)"),
    ("cpu.load", "CPU busy fraction, 0 .. 1"),
    ("audio.level", "Loudness of what's playing, 0 .. 1"),
    ("audio.bass", "Bass band energy, 0 .. 1"),
    ("audio.mid", "Mid band energy, 0 .. 1"),
    ("audio.treble", "Treble band energy, 0 .. 1"),
    ("audio.beat", "1 on a detected beat, then falls"),
    ("clock.hour", "Local time of day, 0 .. 24"),
    (
        "clock.dusk",
        "1 at dusk, falls off over about 2 h either side",
    ),
    (
        "clock.night",
        "0 in the evening .. 1 at the dead of night .. 0 by dawn",
    ),
    ("clock.day", "Daylight, 0 at night .. 1 at noon"),
    ("state.still", "0 running .. 1 still (locked)"),
    ("state.battery", "1 on battery"),
    ("pulse.build_failed", "A build or command failed (decays)"),
    ("pulse.build_passed", "A build passed (decays)"),
];

/// Groups whose values are never gained or smoothed.
const RAW_GROUPS: &[&str] = &["clock", "state"];

/// Turn user input (`build-failed`, `Typing.Rate`) into a signal name.
pub fn normalize(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

/// Is this a valid signal name (`group.name`, lowercase identifier parts)?
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|p| {
            !p.is_empty()
                && p.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
                && p.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
}

#[derive(Debug, Clone)]
struct Slot {
    name: String,
    group: String,
    raw: f32,
    smoother: Smoother,
    /// Pulses: when it fired (bus clock), if ever.
    fired: Option<f64>,
    /// `pulse.x.age` slots point at their pulse.
    age_of: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct SignalBus {
    slots: Vec<Slot>,
    index: HashMap<String, usize>,
    values: Vec<f32>,
    now: f64,
}

impl SignalBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Slot of `name`, registering it (reads 0 until written).
    pub fn slot(&mut self, name: &str) -> usize {
        if let Some(&i) = self.index.get(name) {
            return i;
        }
        let group = name.split('.').next().unwrap_or("").to_string();
        let i = self.slots.len();
        self.slots.push(Slot {
            name: name.to_string(),
            group: group.clone(),
            raw: 0.0,
            smoother: Smoother::new(Smoothing::NONE),
            fired: None,
            age_of: None,
        });
        self.index.insert(name.to_string(), i);
        self.values.push(0.0);
        if group == "pulse" {
            if let Some(base) = name.strip_suffix(".age") {
                let b = self.slot(base);
                self.slots[i].age_of = Some(b);
                self.slots[i].raw = NEVER;
                self.values[i] = NEVER;
            } else {
                self.slot(&format!("{name}.age"));
            }
        }
        i
    }

    /// Write a raw value.
    pub fn set(&mut self, name: &str, value: f32) {
        let i = self.slot(name);
        self.slots[i].raw = if value.is_finite() { value } else { 0.0 };
    }

    /// Fire `pulse.<name>`.
    pub fn pulse(&mut self, name: &str) {
        let full = if name.starts_with("pulse.") {
            name.to_string()
        } else {
            format!("pulse.{name}")
        };
        let i = self.slot(&full);
        self.slots[i].fired = Some(self.now);
    }

    /// The processed value of `name` (after the last update), or 0.
    pub fn get(&self, name: &str) -> f32 {
        self.index.get(name).map_or(0.0, |&i| self.values[i])
    }

    /// The raw value of `name`, or 0.
    pub fn raw(&self, name: &str) -> f32 {
        self.index.get(name).map_or(0.0, |&i| self.slots[i].raw)
    }

    /// Processed values, by slot.
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// `(name, raw, value)` for every signal, for `status`.
    pub fn snapshot(&self) -> Vec<(String, f32, f32)> {
        self.slots
            .iter()
            .zip(&self.values)
            .map(|(s, v)| (s.name.clone(), s.raw, *v))
            .collect()
    }

    /// Advance to `now` (seconds, any monotonic origin) and recompute every
    /// value with `settings`.
    pub fn update(&mut self, now: f64, dt: f32, settings: &Settings) {
        self.now = now;
        for i in 0..self.slots.len() {
            let group = self.slots[i].group.as_str();
            if RAW_GROUPS.contains(&group) {
                self.values[i] = self.slots[i].raw;
                continue;
            }
            let sig = settings.signal(group).cloned().unwrap_or_default();
            let gain = sig.gain
                * if RAW_GROUPS.contains(&group) {
                    1.0
                } else {
                    settings.intensity
                };
            if group == "pulse" {
                let base = self.slots[i].age_of.unwrap_or(i);
                let age = self.slots[base]
                    .fired
                    .map_or(NEVER, |t| (now - t).max(0.0) as f32);
                self.values[i] = if self.slots[i].age_of.is_some() {
                    age
                } else if sig.enabled && age < NEVER {
                    gain * (-age / PULSE_DECAY).exp()
                } else {
                    0.0
                };
                continue;
            }
            let slot = &mut self.slots[i];
            // workspace.id is an identifier, not a level.
            let target = if !sig.enabled {
                0.0
            } else if slot.name.ends_with(".id") {
                slot.raw
            } else {
                slot.raw * gain
            };
            slot.smoother.smoothing = sig.smoothing;
            self.values[i] = slot.smoother.update(target, dt);
        }
    }

    /// Every smoothed value has reached its target and no pulse is still
    /// visibly decaying: nothing would change on screen by itself.
    pub fn settled(&self) -> bool {
        self.slots.iter().zip(&self.values).all(|(s, v)| {
            if s.group == "pulse" {
                s.age_of.is_some() || *v < 1e-3
            } else if RAW_GROUPS.contains(&s.group.as_str()) {
                true
            } else {
                s.smoother.settled(s.raw) || (s.raw == 0.0 && v.abs() < 1e-3)
            }
        })
    }
}

/// Derived time-of-day signals for `hour` (0..24), with the user's dusk and
/// night hours: `(dusk, night, day)`.
pub fn time_of_day(hour: f32, dusk_hour: f32, night_hour: f32) -> (f32, f32, f32) {
    let h = hour.rem_euclid(24.0);
    // Signed distance in hours on the 24 h circle.
    let dist = |a: f32, b: f32| {
        let d = (a - b).rem_euclid(24.0);
        if d > 12.0 { d - 24.0 } else { d }
    };
    let dusk = (-(dist(h, dusk_hour) / 1.6).powi(2)).exp();
    // Night ramps from 2 h after dusk to its peak, then fades by dawn.
    let start = dusk_hour + 2.0;
    let span_in = (night_hour - start).rem_euclid(24.0).max(0.5);
    let since = (h - start).rem_euclid(24.0);
    let dawn = night_hour + 3.0;
    let night = if since <= span_in {
        smooth01(since / span_in)
    } else {
        let after = (h - night_hour).rem_euclid(24.0);
        1.0 - smooth01(after / (dawn - night_hour))
    };
    let day = (-(dist(h, 13.0) / 4.0).powi(2)).exp();
    (dusk, night.clamp(0.0, 1.0), day)
}

fn smooth01(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Layer};

    fn settings(toml: &str) -> Settings {
        Settings::from_config(&Config::from_layers(&[(Layer::User, toml)]).unwrap(), None)
    }

    #[test]
    fn unknown_reads_zero_and_names_normalize() {
        let bus = SignalBus::new();
        assert_eq!(bus.get("nope"), 0.0);
        assert_eq!(normalize("Build-Failed"), "build_failed");
        assert!(valid_name("cpu.load") && valid_name("pulse.build_failed.age"));
        assert!(
            !valid_name("cpu..load") && !valid_name("Cpu") && !valid_name("1x") && !valid_name("")
        );
    }

    #[test]
    fn gain_intensity_and_off() {
        let mut bus = SignalBus::new();
        bus.set("cpu.load", 0.5);
        bus.set("clock.hour", 18.0);
        let s = settings("intensity = 0.5\n[signals.cpu]\ngain = 2\nattack = 0\nrelease = 0\n");
        bus.update(0.0, 0.016, &s);
        assert_eq!(bus.get("cpu.load"), 0.5, "0.5 * gain 2 * intensity 0.5");
        assert_eq!(bus.get("clock.hour"), 18.0, "clock is never gained");
        let s = settings("[signals.cpu]\nenabled = false\nattack = 0\nrelease = 0\n");
        bus.update(0.1, 0.016, &s);
        assert_eq!(bus.get("cpu.load"), 0.0);
        assert_eq!(bus.raw("cpu.load"), 0.5);
    }

    #[test]
    fn smoothing_uses_group_settings() {
        let mut bus = SignalBus::new();
        let s = settings("[signals.typing]\nattack = 1\nrelease = 1\n");
        bus.set("typing.rate", 0.0);
        bus.update(0.0, 0.016, &s);
        bus.set("typing.rate", 1.0);
        bus.update(1.0, 1.0, &s);
        assert!(
            (bus.get("typing.rate") - 0.632).abs() < 1e-3,
            "{}",
            bus.get("typing.rate")
        );
        assert!(!bus.settled());
        for k in 0..400 {
            bus.update(1.0 + k as f64 * 0.05, 0.05, &s);
        }
        assert!(bus.settled());
    }

    #[test]
    fn pulses_decay_and_age() {
        let mut bus = SignalBus::new();
        let s = Settings::default();
        bus.slot("pulse.build_failed");
        bus.update(10.0, 0.0, &s);
        assert_eq!(bus.get("pulse.build_failed"), 0.0);
        assert_eq!(bus.get("pulse.build_failed.age"), NEVER);
        bus.pulse("build_failed");
        bus.update(10.0, 0.0, &s);
        assert_eq!(bus.get("pulse.build_failed"), 1.0);
        bus.update(10.0 + f64::from(PULSE_DECAY), 0.5, &s);
        assert!((bus.get("pulse.build_failed") - (-1.0f32).exp()).abs() < 1e-4);
        assert!((bus.get("pulse.build_failed.age") - PULSE_DECAY).abs() < 1e-4);
        assert!(!bus.settled());
        bus.update(20.0, 0.5, &s);
        assert!(bus.settled());
    }

    #[test]
    fn time_of_day_shape() {
        let (dusk, night, _) = time_of_day(18.0, 18.0, 3.0);
        assert!(dusk > 0.99 && night < 0.01, "{dusk} {night}");
        let (dusk, night, _) = time_of_day(3.0, 18.0, 3.0);
        assert!(dusk < 0.01 && night > 0.99, "{dusk} {night}");
        let (_, night, day) = time_of_day(13.0, 18.0, 3.0);
        assert!(night < 0.01 && day > 0.99);
        let (_, n22, _) = time_of_day(22.0, 18.0, 3.0);
        let (_, n1, _) = time_of_day(1.0, 18.0, 3.0);
        assert!(n22 > 0.0 && n22 < n1 && n1 < 1.0, "{n22} {n1}");
        // Custom hours move it.
        let (dusk, _, _) = time_of_day(20.0, 20.0, 4.0);
        assert!(dusk > 0.99);
    }
}
