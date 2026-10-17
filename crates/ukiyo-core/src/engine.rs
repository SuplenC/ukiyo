//! The engine: a scene, its compiled bindings and params, the signal bus,
//! the scene clock and stillness. Platform-neutral; the daemon and the
//! preview each drive one and hand its [`Frame`]s to a renderer.

use crate::config::{Config, scene_schema};
use crate::expr::Expr;
use crate::scene::{ParamKind, Scene};
use crate::settings::{OnBattery, Settings};
use crate::signals::{SignalBus, time_of_day};
use crate::smooth::Smoother;

/// Floats in the built-in part of the uniform block (see [`UniformLayout`]).
pub const HEADER_FLOATS: usize = 12;
/// Longest step the scene clock takes: after a pause, no jump.
const MAX_DT: f32 = 0.25;

/// Where every uniform lives. The block is a WGSL struct:
///
/// ```text
/// resolution: vec2f, cover_scale: vec2f, cover_center: vec2f,   // floats 0..6
/// time, dt, still, frame, seed, wall: f32,                         // 6..12
/// <colour params>: vec4f ...,                                      // 12..
/// <other params, then bindings>: f32 ...,
/// _padN: f32 ...                                                    // to 16 bytes
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct UniformLayout {
    pub colors: Vec<String>,
    pub scalars: Vec<String>,
}

impl UniformLayout {
    pub fn total_floats(&self) -> usize {
        let n = HEADER_FLOATS + 4 * self.colors.len() + self.scalars.len();
        n.div_ceil(4) * 4
    }

    pub fn scalar_offset(&self, i: usize) -> usize {
        HEADER_FLOATS + 4 * self.colors.len() + i
    }

    /// The WGSL declaration of the `Ukiyo` struct.
    pub fn wgsl(&self) -> String {
        let mut s = String::from(
            "struct Ukiyo {\n    resolution: vec2<f32>,\n    cover_scale: vec2<f32>,\n    cover_center: vec2<f32>,\n    time: f32,\n    dt: f32,\n    still: f32,\n    frame: f32,\n    seed: f32,\n    wall: f32,\n",
        );
        for c in &self.colors {
            s += &format!("    {c}: vec4<f32>,\n");
        }
        for c in &self.scalars {
            s += &format!("    {c}: f32,\n");
        }
        let used = HEADER_FLOATS + 4 * self.colors.len() + self.scalars.len();
        for i in used..self.total_floats() {
            s += &format!("    _pad{i}: f32,\n");
        }
        s += "}\n";
        s
    }
}

struct Binding {
    expr: Expr,
    smoother: Smoother,
    min: f32,
    max: f32,
}

/// One frame's worth of uniforms. Floats 0..6 (resolution and cover) are
/// per output and filled in by the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub uniforms: Vec<f32>,
    pub time: f32,
    pub still: f32,
}

pub struct Engine {
    pub scene: Scene,
    pub bus: SignalBus,
    pub layout: UniformLayout,
    settings: Settings,
    params: Vec<[f32; 4]>,
    param_kinds: Vec<ParamKind>,
    bindings: Vec<Binding>,
    binding_names: Vec<String>,
    slots: Vec<f32>,
    /// Unknown names in binding expressions, for a warning.
    pub unknown_names: Vec<String>,
    last_now: Option<f64>,
    start: Option<f64>,
    time: f64,
    frame: u64,
    still: f32,
    want_still: bool,
    on_battery: bool,
    clock_override: Option<f32>,
    last_tail: Vec<f32>,
    changed: bool,
}

impl Engine {
    pub fn new(scene: Scene, config: &Config) -> Engine {
        let m = &scene.manifest;
        let mut colors = Vec::new();
        let mut scalars = Vec::new();
        let mut param_kinds = Vec::new();
        for (name, p) in &m.params {
            param_kinds.push(p.kind);
            if p.kind == ParamKind::Color {
                colors.push(name.clone());
            } else {
                scalars.push(name.clone());
            }
        }
        let n_params = m.params.len();
        let binding_names: Vec<String> = m.bindings.iter().map(|(n, _)| n.clone()).collect();
        scalars.extend(binding_names.iter().cloned());

        let mut bus = SignalBus::new();
        for name in [
            "clock.hour",
            "clock.dusk",
            "clock.night",
            "clock.day",
            "state.still",
            "state.battery",
        ] {
            bus.slot(name);
        }
        let base = n_params + binding_names.len();
        let mut unknown_names = Vec::new();
        let mut bindings = Vec::new();
        for (i, (name, spec)) in m.bindings.iter().enumerate() {
            // Validated when the manifest was parsed.
            let expr = Expr::parse(spec.expr()).unwrap_or(Expr::Num(0.0));
            let (expr, unknown) = expr.compile(&mut |n: &str| {
                if let Some(p) = n.strip_prefix("param.") {
                    return m.params.iter().position(|(pn, _)| pn == p);
                }
                if let Some(j) = binding_names[..i].iter().position(|b| b == n) {
                    return Some(n_params + j);
                }
                if n.contains('.') && crate::signals::valid_name(n) {
                    return Some(base + bus.slot(n));
                }
                None
            });
            for u in unknown {
                unknown_names.push(format!("{name}: `{u}`"));
            }
            let (min, max) = spec.range();
            bindings.push(Binding {
                expr,
                smoother: Smoother::new(spec.smoothing()),
                min,
                max,
            });
        }
        let mut e = Engine {
            layout: UniformLayout { colors, scalars },
            settings: Settings::default(),
            params: vec![[0.0; 4]; n_params],
            param_kinds,
            bindings,
            binding_names,
            slots: Vec::new(),
            unknown_names,
            bus,
            last_now: None,
            start: None,
            time: 0.0,
            frame: 0,
            still: 0.0,
            want_still: false,
            on_battery: false,
            clock_override: None,
            last_tail: Vec::new(),
            changed: true,
            scene,
        };
        e.apply_config(config);
        e
    }

    /// Re-read settings and params (hot reload, `config set`).
    pub fn apply_config(&mut self, config: &Config) {
        self.settings = Settings::from_config(config, Some(&self.scene.id));
        for (i, def) in scene_schema(&self.scene.id, &self.scene.manifest)
            .iter()
            .enumerate()
        {
            let (v, _) = config.value(def, None);
            let (_, spec) = &self.scene.manifest.params[i];
            self.params[i] = spec
                .uniform(&v)
                .or_else(|| spec.uniform(&spec.default))
                .unwrap_or([0.0; 4]);
        }
        self.changed = true;
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Lock: ease to a standstill over `lock.transition` (or back).
    pub fn set_still(&mut self, still: bool) {
        self.want_still = still;
        self.changed = true;
    }

    pub fn set_on_battery(&mut self, on: bool) {
        self.on_battery = on;
        self.bus.set("state.battery", if on { 1.0 } else { 0.0 });
        self.changed = true;
    }

    /// Fake the time of day (preview, scripts). None: settings / real clock.
    pub fn set_clock_override(&mut self, hour: Option<f32>) {
        self.clock_override = hour;
    }

    /// Is the scene asked to stand still (lock, motion off, battery rule)?
    pub fn target_still(&self) -> bool {
        self.want_still
            || !self.settings.motion
            || (self.on_battery && self.settings.on_battery == OnBattery::Still)
    }

    /// How still it is right now, 0..1.
    pub fn still(&self) -> f32 {
        self.still
    }

    /// On the way to a standstill or out of one: ticks still move it.
    pub fn easing(&self) -> bool {
        self.still != if self.target_still() { 1.0 } else { 0.0 }
    }

    /// Frame rate cap and render scale factor (user × scene × battery).
    pub fn fps_and_scale(&self) -> (f32, f32) {
        let r = &self.scene.manifest.render;
        let (fps, scale) = self.settings.effective_fps_scale(r.fps, self.on_battery);
        (fps, (scale * r.scale).clamp(0.1, 2.0))
    }

    pub fn frame_interval(&self) -> f64 {
        1.0 / f64::from(self.fps_and_scale().0.max(1.0))
    }

    /// Should another frame follow the last one? False once the scene is
    /// fully still, or for a non-animated scene once nothing changes.
    pub fn wants_frame(&self) -> bool {
        if self.easing() {
            return true;
        }
        if self.still >= 1.0 {
            return self.changed;
        }
        self.scene.manifest.render.animated || self.changed || !self.bus.settled()
    }

    /// Local hour now (or the pinned/overridden one).
    pub fn hour(&self) -> f32 {
        if let Some(h) = self.clock_override.or(self.settings.clock.pinned) {
            return h;
        }
        use chrono::Timelike;
        let t = chrono::Local::now();
        t.hour() as f32 + t.minute() as f32 / 60.0 + t.second() as f32 / 3600.0
    }

    /// Advance to `now` (seconds, monotonic) and produce the uniforms.
    pub fn tick(&mut self, now: f64) -> Frame {
        let dt = self
            .last_now
            .map_or(0.0, |l| ((now - l) as f32).clamp(0.0, MAX_DT));
        self.last_now = Some(now);
        let start = *self.start.get_or_insert(now);

        let target = if self.target_still() { 1.0 } else { 0.0 };
        let tr = self.settings.lock_transition.max(0.0);
        self.still = if tr <= 0.0 {
            target
        } else if self.still < target {
            (self.still + dt / tr).min(target)
        } else {
            (self.still - dt / tr).max(target)
        };
        // Ease: time slows smoothly to a stop.
        let s = self.still;
        let speed = 1.0 - s * s * (3.0 - 2.0 * s);
        self.time += f64::from(dt * speed);

        let hour = self.hour();
        let c = &self.settings.clock;
        let (dusk, night, day) = time_of_day(hour, c.dusk, c.night);
        self.bus.set("clock.hour", hour);
        self.bus.set("clock.dusk", dusk);
        self.bus.set("clock.night", night);
        self.bus.set("clock.day", day);
        self.bus.set("state.still", s);
        // A still scene doesn't react any more: its signals settle where they are.
        let sig_dt = dt * speed;
        let settings = self.settings.clone();
        self.bus.update(now, sig_dt, &settings);

        let n_params = self.params.len();
        self.slots.clear();
        // `param.<colour>` reads its red channel in expressions.
        self.slots.extend(self.params.iter().map(|p| p[0]));
        self.slots.resize(n_params + self.bindings.len(), 0.0);
        self.slots.extend_from_slice(self.bus.values());
        for i in 0..self.bindings.len() {
            let b = &mut self.bindings[i];
            let v = b.expr.eval(&self.slots).clamp(b.min, b.max);
            let v = b.smoother.update(v, sig_dt);
            self.slots[n_params + i] = v;
        }

        let mut u = vec![0.0f32; self.layout.total_floats()];
        u[6] = self.time as f32;
        u[7] = dt;
        u[8] = s;
        u[9] = self.frame as f32;
        u[10] = seed(&self.scene.id);
        u[11] = (now - start) as f32;
        let mut ci = 0;
        let mut si = 0;
        for (p, kind) in self.params.iter().zip(&self.param_kinds) {
            if *kind == ParamKind::Color {
                let o = HEADER_FLOATS + 4 * ci;
                u[o..o + 4].copy_from_slice(p);
                ci += 1;
            } else {
                u[self.layout.scalar_offset(si)] = p[0];
                si += 1;
            }
        }
        for i in 0..self.bindings.len() {
            u[self.layout.scalar_offset(si + i)] = self.slots[n_params + i];
        }
        self.frame += 1;
        let tail = &u[HEADER_FLOATS..];
        self.changed = self.last_tail.len() != tail.len()
            || self
                .last_tail
                .iter()
                .zip(tail)
                .any(|(a, b)| (a - b).abs() > 1e-4);
        self.last_tail.clear();
        self.last_tail.extend_from_slice(tail);
        Frame {
            uniforms: u,
            time: self.time as f32,
            still: s,
        }
    }

    /// Current binding values, by name (for status and the preview).
    pub fn binding_values(&self) -> Vec<(String, f32)> {
        let n = self.params.len();
        self.binding_names
            .iter()
            .enumerate()
            .map(|(i, name)| (name.clone(), self.slots.get(n + i).copied().unwrap_or(0.0)))
            .collect()
    }

    /// Mark that something outside the engine changed (signal written):
    /// a non-animated scene draws again.
    pub fn poke(&mut self) {
        self.changed = true;
    }
}

fn seed(id: &str) -> f32 {
    let h = id.bytes().fold(2166136261u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(16777619)
    });
    (h % 10007) as f32 / 10007.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Layer;
    use crate::scene::Manifest;

    fn scene(toml: &str) -> Scene {
        Scene {
            id: "t".into(),
            dir: ".".into(),
            manifest: Manifest::parse(toml).unwrap(),
            shader_source: String::new(),
        }
    }

    const TOML: &str = r##"
[scene]
name = "T"
[render]
fps = 30
[params.tint]
type = "color"
label = "Tint"
default = "#ffffff"
[params.rain]
type = "float"
label = "Rain"
default = 0.5
[bindings]
flicker = "typing.rate * 0.8 + cpu.load * 0.3"
twice = "flicker * 2 + param.rain"
night = "clock.night"
"##;

    #[test]
    fn layout_and_uniforms() {
        let cfg = Config::from_layers(&[(Layer::User, "clock.mode = \"pinned\"\nclock.hour = 3\n[signals.typing]\nattack = 0\nrelease = 0\n[signals.cpu]\nattack = 0\nrelease = 0\n")]).unwrap();
        let mut e = Engine::new(scene(TOML), &cfg);
        assert_eq!(e.layout.colors, ["tint"]);
        assert_eq!(e.layout.scalars, ["rain", "flicker", "twice", "night"]);
        assert_eq!(e.layout.total_floats(), 20);
        let w = e.layout.wgsl();
        assert!(
            w.contains("tint: vec4<f32>") && w.contains("night: f32") && !w.contains("_pad"),
            "{w}"
        );
        assert!(e.unknown_names.is_empty(), "{:?}", e.unknown_names);
        e.bus.set("typing.rate", 1.0);
        e.bus.set("cpu.load", 1.0);
        let f = e.tick(0.0);
        let o = |i| e.layout.scalar_offset(i);
        assert_eq!(&f.uniforms[12..16], &[1.0, 1.0, 1.0, 1.0], "white, linear");
        assert_eq!(f.uniforms[o(0)], 0.5);
        assert!((f.uniforms[o(1)] - 1.1).abs() < 1e-5);
        assert!(
            (f.uniforms[o(2)] - 2.7).abs() < 1e-5,
            "a binding reads an earlier one and a param"
        );
        assert!(f.uniforms[o(3)] > 0.99, "pinned to 3 am");
    }

    #[test]
    fn config_overrides_params_live() {
        let mut e = Engine::new(scene(TOML), &Config::default());
        let f = e.tick(0.0);
        assert_eq!(f.uniforms[e.layout.scalar_offset(0)], 0.5);
        e.apply_config(&Config::from_layers(&[(Layer::User, "[scenes.t]\nrain = 0.9\n")]).unwrap());
        let f = e.tick(0.1);
        assert!((f.uniforms[e.layout.scalar_offset(0)] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn going_still_slows_time_to_a_stop() {
        let cfg = Config::from_layers(&[(Layer::User, "lock.transition = 1.0\n")]).unwrap();
        let mut e = Engine::new(scene(TOML), &cfg);
        let mut now = 0.0;
        e.tick(now);
        for _ in 0..30 {
            now += 1.0 / 30.0;
            e.tick(now);
        }
        assert!(e.wants_frame());
        let t_running = e.tick(now).time;
        e.set_still(true);
        let mut last = t_running;
        for _ in 0..40 {
            now += 1.0 / 30.0;
            let f = e.tick(now);
            assert!(f.time >= last);
            last = f.time;
        }
        assert_eq!(e.still(), 1.0);
        let f = e.tick(now + 1.0);
        assert_eq!(f.time, last, "time stopped");
        assert!(!e.wants_frame(), "0 fps once still");
        e.set_still(false);
        assert!(e.wants_frame());
    }

    #[test]
    fn going_still_at_four_ticks_a_second_takes_the_transition() {
        let cfg = Config::from_layers(&[(Layer::User, "lock.transition = 1.5\n")]).unwrap();
        let mut e = Engine::new(scene(TOML), &cfg);
        e.tick(0.0);
        assert!(!e.easing());
        e.set_still(true);
        assert!(e.easing());
        for i in 1..=5 {
            e.tick(f64::from(i) * 0.25);
        }
        assert!(e.easing(), "1.25 s in, not there yet");
        e.tick(1.5);
        assert_eq!(e.still(), 1.0);
        assert!(!e.easing());
        e.set_still(false);
        assert!(e.easing(), "and back");
    }

    #[test]
    fn motion_off_and_battery_still() {
        let cfg =
            Config::from_layers(&[(Layer::User, "performance.on_battery = \"still\"\n")]).unwrap();
        let mut e = Engine::new(scene(TOML), &cfg);
        assert!(!e.target_still());
        e.set_on_battery(true);
        assert!(e.target_still());
        let cfg = Config::from_layers(&[(Layer::User, "performance.motion = false\n")]).unwrap();
        e.apply_config(&cfg);
        e.set_on_battery(false);
        assert!(e.target_still());
        let cfg = Config::from_layers(&[(
            Layer::User,
            "performance.on_battery = \"reduced\"\nperformance.fps = 24\n",
        )])
        .unwrap();
        e.apply_config(&cfg);
        e.set_on_battery(true);
        assert_eq!(e.fps_and_scale(), (12.0, 0.75));
    }
}
