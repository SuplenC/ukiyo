//! Typed view of the [`Config`](crate::config::Config): what the engine and
//! the daemon read every frame, resolved once per config change.

use toml::Value;

use crate::config::{Config, OptDef, SIGNALS, global_schema};
use crate::smooth::Smoothing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnBattery {
    Full,
    Reduced,
    Still,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SignalSettings {
    pub enabled: bool,
    pub gain: f32,
    pub smoothing: Smoothing,
}

impl Default for SignalSettings {
    fn default() -> Self {
        SignalSettings {
            enabled: true,
            gain: 1.0,
            smoothing: Smoothing::NONE,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClockSettings {
    /// Some(hour) when the time of day is pinned.
    pub pinned: Option<f32>,
    pub dusk: f32,
    pub night: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub scene: Option<String>,
    pub intensity: f32,
    pub motion: bool,
    pub fps: f32,
    pub scale: f32,
    pub on_battery: OnBattery,
    pub pause_on_fullscreen: bool,
    /// Seconds; 0 = never.
    pub idle_timeout: f32,
    pub clock: ClockSettings,
    pub lock_snapshot: bool,
    pub lock_transition: f32,
    /// Per signal group (`cursor`, `workspace`, `typing`, `cpu`, `audio`, `pulse`).
    pub signals: Vec<(String, SignalSettings)>,
    pub cursor_source: String,
    pub typing_source: String,
    pub helper_socket: String,
    pub cpu_window: f32,
    pub audio_device: String,
    pub build_commands: Vec<String>,
    pub build_min_duration: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::from_config(&Config::default(), None)
    }
}

impl Settings {
    /// Resolve every option for `scene` (per-scene overrides apply).
    pub fn from_config(cfg: &Config, scene: Option<&str>) -> Settings {
        let schema = global_schema();
        let get = |key: &str| -> Value {
            let def: &OptDef = schema
                .iter()
                .find(|d| d.key == key)
                .unwrap_or_else(|| panic!("no option {key}"));
            cfg.value(def, scene).0
        };
        let num = |key: &str| get(key).as_float().unwrap_or(0.0) as f32;
        let flag = |key: &str| get(key).as_bool().unwrap_or(false);
        let text = |key: &str| get(key).as_str().unwrap_or_default().to_string();
        let signals = SIGNALS
            .iter()
            .map(|(name, ..)| {
                let k = |x: &str| format!("signals.{name}.{x}");
                let smoothing = if *name == "pulse" {
                    Smoothing::NONE
                } else {
                    Smoothing {
                        attack: num(&k("attack")),
                        release: num(&k("release")),
                    }
                };
                (
                    name.to_string(),
                    SignalSettings {
                        enabled: flag(&k("enabled")),
                        gain: num(&k("gain")),
                        smoothing,
                    },
                )
            })
            .collect();
        let scene_id = text("scene");
        Settings {
            scene: (!scene_id.is_empty()).then_some(scene_id),
            intensity: num("intensity"),
            motion: flag("performance.motion"),
            fps: num("performance.fps"),
            scale: num("performance.scale"),
            on_battery: match text("performance.on_battery").as_str() {
                "full" => OnBattery::Full,
                "still" => OnBattery::Still,
                _ => OnBattery::Reduced,
            },
            pause_on_fullscreen: flag("performance.pause_on_fullscreen"),
            idle_timeout: num("performance.idle_timeout"),
            clock: ClockSettings {
                pinned: (text("clock.mode") == "pinned").then(|| num("clock.hour")),
                dusk: num("clock.dusk"),
                night: num("clock.night"),
            },
            lock_snapshot: flag("lock.snapshot"),
            lock_transition: num("lock.transition"),
            signals,
            cursor_source: text("signals.cursor.source"),
            typing_source: text("signals.typing.source"),
            helper_socket: text("signals.typing.helper_socket"),
            cpu_window: num("signals.cpu.window"),
            audio_device: text("signals.audio.device"),
            build_commands: get("signals.build.commands")
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            build_min_duration: num("signals.build.min_duration"),
        }
    }

    pub fn signal(&self, group: &str) -> Option<&SignalSettings> {
        self.signals
            .iter()
            .find(|(n, _)| n == group)
            .map(|(_, s)| s)
    }

    /// Is a finished command a build, per `signals.build.*`?
    pub fn is_build(&self, command: &str, duration: f32) -> bool {
        let first = command
            .split_whitespace()
            .find(|w| !w.contains('='))
            .unwrap_or("");
        let first = first.rsplit('/').next().unwrap_or(first);
        self.build_commands.iter().any(|c| c == first)
            || (self.build_min_duration > 0.0 && duration >= self.build_min_duration)
    }

    /// Frame-rate cap and render-scale factor given the power source.
    pub fn effective_fps_scale(&self, scene_fps: f32, on_battery: bool) -> (f32, f32) {
        let fps = scene_fps.min(self.fps);
        match (on_battery, self.on_battery) {
            (true, OnBattery::Reduced) => (fps * 0.5, self.scale * 0.75),
            _ => (fps, self.scale),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Layer;

    #[test]
    fn defaults() {
        let s = Settings::default();
        assert_eq!(s.intensity, 1.0);
        assert_eq!(s.on_battery, OnBattery::Reduced);
        assert_eq!(s.clock.pinned, None);
        assert_eq!(
            s.signal("typing").unwrap().smoothing,
            Smoothing {
                attack: 0.15,
                release: 1.5
            }
        );
        assert_eq!(s.signal("pulse").unwrap().smoothing, Smoothing::NONE);
        assert!(s.build_commands.contains(&"cargo".to_string()));
        assert_eq!(s.scene, None);
    }

    #[test]
    fn from_layers_with_scene_override() {
        let cfg = Config::from_layers(&[(
            Layer::User,
            "scene = \"gate\"\nclock.mode = \"pinned\"\nclock.hour = 3\n[signals.cpu]\nenabled = false\n[scenes.gate.performance]\nfps = 24\n",
        )])
        .unwrap();
        let s = Settings::from_config(&cfg, Some("gate"));
        assert_eq!(s.scene.as_deref(), Some("gate"));
        assert_eq!(s.clock.pinned, Some(3.0));
        assert!(!s.signal("cpu").unwrap().enabled);
        assert_eq!(s.fps, 24.0);
        assert_eq!(Settings::from_config(&cfg, Some("other")).fps, 60.0);
    }

    #[test]
    fn builds_and_battery() {
        let s = Settings::default();
        assert!(s.is_build("cargo build --release", 1.0));
        assert!(s.is_build("RUSTFLAGS=-g /usr/bin/make -j8", 1.0));
        assert!(!s.is_build("ls -la", 1.0));
        assert!(s.is_build("sleep 30", 30.0));
        assert_eq!(s.effective_fps_scale(30.0, false), (30.0, 1.0));
        assert_eq!(s.effective_fps_scale(30.0, true), (15.0, 0.75));
        assert_eq!(s.effective_fps_scale(90.0, false), (60.0, 1.0));
    }
}
