//! User configuration: every knob ukiyo has, layered and self-describing.
//!
//! Files, lowest precedence first (all optional):
//!
//! 1. `$XDG_CONFIG_DIRS/ukiyo/config.toml` (system, e.g. `/etc/xdg`)
//! 2. `$XDG_CONFIG_HOME/ukiyo/defaults.toml`: declarative defaults, the file
//!    a home-manager module or a dotfiles repo writes (read-only is fine)
//! 3. `$XDG_CONFIG_HOME/ukiyo/config.toml`: the user's own; what
//!    `ukiyo config set` writes
//!
//! Every key may also appear under `[scenes.<id>]`, where it applies to that
//! scene only. Lookup order: user scene, user global, defaults scene,
//! defaults global, system scene, system global, built-in default. Scene
//! params (`[params]` in `scene.toml`) live at `scenes.<id>.<param>`.
//!
//! [`schema`] lists every option with type, range, default and label; the
//! CLI prints it as JSON so a settings UI can be generated from it.

use std::path::{Path, PathBuf};

use serde::Serialize;
use toml::{Table, Value};

use crate::scene::{Manifest, ParamKind, parse_color};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Kind {
    Bool,
    Float {
        min: f32,
        max: f32,
        step: f32,
        #[serde(skip_serializing_if = "String::is_empty")]
        unit: String,
    },
    Choice {
        options: Vec<String>,
    },
    Color,
    String,
    List,
}

/// One option in the schema.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OptDef {
    pub key: String,
    #[serde(flatten)]
    pub kind: Kind,
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub help: String,
    pub group: String,
    #[serde(serialize_with = "ser_value")]
    pub default: Value,
}

fn ser_value<S: serde::Serializer>(v: &Value, s: S) -> Result<S::Ok, S::Error> {
    to_json(v).serialize(s)
}

/// TOML value to JSON (for status and schema output).
pub fn to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Integer(i) => serde_json::Value::from(*i),
        Value::Float(f) => serde_json::Number::from_f64(*f)
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Datetime(d) => serde_json::Value::String(d.to_string()),
        Value::Array(a) => serde_json::Value::Array(a.iter().map(to_json).collect()),
        Value::Table(t) => {
            serde_json::Value::Object(t.iter().map(|(k, v)| (k.clone(), to_json(v))).collect())
        }
    }
}

/// JSON value to TOML (IPC `config set` sends JSON).
pub fn from_json(v: &serde_json::Value) -> Option<Value> {
    Some(match v {
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::Integer(i),
            None => Value::Float(n.as_f64()?),
        },
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(a) => {
            Value::Array(a.iter().map(from_json).collect::<Option<_>>()?)
        }
        _ => return None,
    })
}

impl OptDef {
    fn new(key: &str, kind: Kind, default: Value, label: &str, group: &str, help: &str) -> Self {
        OptDef {
            key: key.into(),
            kind,
            default,
            label: label.into(),
            group: group.into(),
            help: help.into(),
        }
    }

    /// Check (and normalise) a value for this option: numbers are clamped to
    /// the range, integers become floats, a choice must be one of the options.
    pub fn coerce(&self, v: &Value) -> Result<Value, String> {
        match &self.kind {
            Kind::Bool => v
                .as_bool()
                .map(Value::Boolean)
                .ok_or_else(|| "expected true or false".into()),
            Kind::Float { min, max, .. } => {
                let x = match v {
                    Value::Float(f) => *f,
                    Value::Integer(i) => *i as f64,
                    _ => return Err("expected a number".into()),
                };
                if !x.is_finite() {
                    return Err("expected a finite number".into());
                }
                Ok(Value::Float(x.clamp(f64::from(*min), f64::from(*max))))
            }
            Kind::Choice { options } => match v.as_str() {
                Some(s) if options.iter().any(|o| o == s) => Ok(v.clone()),
                _ => Err(format!("expected one of: {}", options.join(", "))),
            },
            Kind::Color => match v.as_str().and_then(parse_color) {
                Some(_) => Ok(v.clone()),
                None => Err("expected a colour like \"#ff2040\"".into()),
            },
            Kind::String => v
                .as_str()
                .map(|s| Value::String(s.into()))
                .ok_or_else(|| "expected a string".into()),
            Kind::List => match v {
                Value::Array(a) if a.iter().all(Value::is_str) => Ok(v.clone()),
                _ => Err("expected a list of strings".into()),
            },
        }
    }

    /// Parse a command-line string for this option.
    pub fn parse_cli(&self, s: &str) -> Result<Value, String> {
        let v = match &self.kind {
            Kind::Bool => match s.to_ascii_lowercase().as_str() {
                "true" | "on" | "yes" | "1" => Value::Boolean(true),
                "false" | "off" | "no" | "0" => Value::Boolean(false),
                _ => return Err("expected true/false (or on/off)".into()),
            },
            Kind::Float { .. } => Value::Float(
                s.trim()
                    .parse::<f64>()
                    .map_err(|_| format!("`{s}` isn't a number"))?,
            ),
            Kind::List => Value::Array(
                s.split(',')
                    .map(str::trim)
                    .filter(|x| !x.is_empty())
                    .map(|x| Value::String(x.into()))
                    .collect(),
            ),
            _ => Value::String(s.into()),
        };
        self.coerce(&v)
    }
}

/// The signal sources and their default smoothing (attack, release).
pub const SIGNALS: &[(&str, &str, f32, f32)] = &[
    ("cursor", "Cursor parallax", 0.25, 0.25),
    ("workspace", "Workspace drift", 0.7, 0.7),
    ("typing", "Typing", 0.15, 1.5),
    ("cpu", "CPU load", 1.0, 3.0),
    ("audio", "Music", 0.03, 0.18),
    ("pulse", "Pulses (build results)", 0.0, 0.0),
];

fn float(min: f32, max: f32, step: f32, unit: &str) -> Kind {
    Kind::Float {
        min,
        max,
        step,
        unit: unit.into(),
    }
}

fn choice(options: &[&str]) -> Kind {
    Kind::Choice {
        options: options.iter().map(|s| s.to_string()).collect(),
    }
}

fn f(v: f64) -> Value {
    Value::Float(v)
}

fn s(v: &str) -> Value {
    Value::String(v.into())
}

/// Every global option.
pub fn global_schema() -> Vec<OptDef> {
    let mut o = vec![
        OptDef::new(
            "scene",
            Kind::String,
            s(""),
            "Scene",
            "General",
            "Id of the scene to show. Empty: none.",
        ),
        OptDef::new(
            "intensity",
            float(0.0, 2.0, 0.05, "×"),
            f(1.0),
            "Reactivity",
            "General",
            "Master gain on every reaction (cursor, typing, CPU, music, pulses). 0 is a calm, still-reacting-to-the-clock wallpaper.",
        ),
        OptDef::new(
            "performance.motion",
            Kind::Bool,
            Value::Boolean(true),
            "Animate",
            "Performance",
            "Off: draw one frame and hold it.",
        ),
        OptDef::new(
            "performance.fps",
            float(5.0, 144.0, 1.0, "fps"),
            f(60.0),
            "Frame rate cap",
            "Performance",
            "Scenes ask for their own rate; this caps it.",
        ),
        OptDef::new(
            "performance.scale",
            float(0.25, 1.0, 0.05, "×"),
            f(1.0),
            "Render scale",
            "Performance",
            "Multiplies the scene's internal resolution. Lower is softer and cheaper.",
        ),
        OptDef::new(
            "performance.on_battery",
            choice(&["full", "reduced", "still"]),
            s("reduced"),
            "On battery",
            "Performance",
            "full: as on AC. reduced: half the frame rate, 0.75 scale. still: hold a frame.",
        ),
        OptDef::new(
            "performance.pause_on_fullscreen",
            Kind::Bool,
            Value::Boolean(true),
            "Pause under fullscreen",
            "Performance",
            "Stop drawing while a fullscreen window covers the output.",
        ),
        OptDef::new(
            "performance.idle_timeout",
            float(0.0, 3600.0, 30.0, "s"),
            f(600.0),
            "Pause when idle",
            "Performance",
            "Seconds without input before drawing stops. 0: never.",
        ),
        OptDef::new(
            "clock.mode",
            choice(&["follow", "pinned"]),
            s("follow"),
            "Time of day",
            "Time of day",
            "follow: the real clock. pinned: always `clock.hour`.",
        ),
        OptDef::new(
            "clock.hour",
            float(0.0, 24.0, 0.25, "h"),
            f(22.0),
            "Pinned hour",
            "Time of day",
            "Used when the time of day is pinned.",
        ),
        OptDef::new(
            "clock.dusk",
            float(14.0, 22.0, 0.25, "h"),
            f(18.0),
            "Dusk",
            "Time of day",
            "When dusk peaks.",
        ),
        OptDef::new(
            "clock.night",
            float(0.0, 6.0, 0.25, "h"),
            f(3.0),
            "Dead of night",
            "Time of day",
            "When the night is deepest.",
        ),
        OptDef::new(
            "lock.snapshot",
            Kind::Bool,
            Value::Boolean(true),
            "Lock screen frame",
            "Lock screen",
            "When going still, save the frame for the lock screen (`ukiyo status` has the paths).",
        ),
        OptDef::new(
            "lock.transition",
            float(0.0, 5.0, 0.1, "s"),
            f(1.5),
            "Time to go still",
            "Lock screen",
            "How long the scene takes to settle when told to go still.",
        ),
    ];
    for (name, label, attack, release) in SIGNALS {
        let g = label.to_string();
        let k = |x: &str| format!("signals.{name}.{x}");
        o.push(OptDef::new(
            &k("enabled"),
            Kind::Bool,
            Value::Boolean(true),
            "On",
            &g,
            "",
        ));
        o.push(OptDef::new(
            &k("gain"),
            float(0.0, 3.0, 0.05, "×"),
            f(1.0),
            "Strength",
            &g,
            "",
        ));
        if *name != "pulse" {
            o.push(OptDef::new(
                &k("attack"),
                float(0.0, 5.0, 0.01, "s"),
                f(f64::from(*attack)),
                "Attack",
                &g,
                "Smoothing while the signal rises.",
            ));
            o.push(OptDef::new(
                &k("release"),
                float(0.0, 10.0, 0.05, "s"),
                f(f64::from(*release)),
                "Release",
                &g,
                "Smoothing while it falls.",
            ));
        }
        match *name {
            "cursor" => o.push(OptDef::new(
                &k("source"),
                choice(&["auto", "hyprland", "pointer"]),
                s("auto"),
                "Source",
                &g,
                "hyprland: the compositor's cursor everywhere. pointer: only over the bare desktop. auto: hyprland if it's there.",
            )),
            "typing" => {
                o.push(OptDef::new(
                    &k("source"),
                    choice(&["auto", "idle", "helper"]),
                    s("auto"),
                    "Source",
                    &g,
                    "idle: unprivileged, from the compositor's idle notifications. helper: exact rate from ukiyo-activityd. auto: helper if its socket answers.",
                ));
                o.push(OptDef::new(
                    &k("helper_socket"),
                    Kind::String,
                    s("/run/ukiyo-activity/socket"),
                    "Helper socket",
                    &g,
                    "Where ukiyo-activityd listens.",
                ));
            }
            "cpu" => o.push(OptDef::new(&k("window"), float(0.25, 5.0, 0.25, "s"), f(1.0), "Averaging window", &g, "")),
            "audio" => o.push(OptDef::new(
                &k("device"),
                Kind::String,
                s(""),
                "Sink",
                &g,
                "PipeWire sink whose monitor is analysed. Empty: the default sink. Never a microphone.",
            )),
            "pulse" => {
                o.push(OptDef::new(
                    "signals.build.commands",
                    Kind::List,
                    Value::Array(["cargo", "just", "make", "ninja", "cmake", "meson", "npm", "pnpm", "go", "gradle", "mvn", "nix"].map(s).to_vec()),
                    "Build commands",
                    &g,
                    "A finished command whose first word is one of these sends build-passed or build-failed.",
                ));
                o.push(OptDef::new(
                    "signals.build.min_duration",
                    float(0.0, 600.0, 1.0, "s"),
                    f(10.0),
                    "Any command longer than",
                    &g,
                    "Other commands count as builds when they ran at least this long. 0: never.",
                ));
            }
            _ => {}
        }
    }
    o
}

/// The options a scene declares, at `scenes.<id>.<param>`.
pub fn scene_schema(id: &str, m: &Manifest) -> Vec<OptDef> {
    m.params
        .iter()
        .map(|(name, p)| {
            let kind = match p.kind {
                ParamKind::Bool => Kind::Bool,
                ParamKind::Color => Kind::Color,
                ParamKind::Choice => Kind::Choice {
                    options: p.options.clone(),
                },
                ParamKind::Float => {
                    let (min, max) = (p.min.unwrap_or(0.0), p.max.unwrap_or(1.0));
                    Kind::Float {
                        min,
                        max,
                        step: p.step.unwrap_or((max - min) / 100.0),
                        unit: String::new(),
                    }
                }
            };
            let group = if p.group.is_empty() {
                m.scene.name.clone()
            } else {
                format!("{}: {}", m.scene.name, p.group)
            };
            OptDef {
                key: format!("scenes.{id}.{name}"),
                kind,
                default: p.default.clone(),
                label: p.label.clone(),
                help: p.help.clone(),
                group,
            }
        })
        .collect()
}

/// Every option: global ones, then the scene's.
pub fn schema(scene: Option<(&str, &Manifest)>) -> Vec<OptDef> {
    let mut o = global_schema();
    if let Some((id, m)) = scene {
        o.extend(scene_schema(id, m));
    }
    o
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Layer {
    System,
    Defaults,
    User,
}

/// Where the config files are.
#[derive(Debug, Clone, PartialEq)]
pub struct Paths {
    pub system: Vec<PathBuf>,
    pub defaults: PathBuf,
    pub user: PathBuf,
}

impl Paths {
    /// From the XDG environment.
    pub fn from_env() -> Paths {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        let dirs = std::env::var("XDG_CONFIG_DIRS").unwrap_or_default();
        let mut system: Vec<PathBuf> = dirs
            .split(':')
            .filter(|d| d.starts_with('/'))
            .map(|d| Path::new(d).join("ukiyo/config.toml"))
            .collect();
        if system.is_empty() {
            system.push(PathBuf::from("/etc/xdg/ukiyo/config.toml"));
        }
        // XDG_CONFIG_DIRS is most important first; we apply lowest first.
        system.reverse();
        Paths {
            system,
            defaults: config_home.join("ukiyo/defaults.toml"),
            user: config_home.join("ukiyo/config.toml"),
        }
    }

    pub fn in_dir(dir: &Path) -> Paths {
        Paths {
            system: vec![],
            defaults: dir.join("defaults.toml"),
            user: dir.join("config.toml"),
        }
    }

    fn all(&self) -> Vec<(Layer, &Path)> {
        let mut v: Vec<(Layer, &Path)> = self
            .system
            .iter()
            .map(|p| (Layer::System, p.as_path()))
            .collect();
        v.push((Layer::Defaults, &self.defaults));
        v.push((Layer::User, &self.user));
        v
    }
}

/// The merged configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Lowest precedence first.
    layers: Vec<(Layer, Table)>,
}

impl Config {
    /// Read every layer. Missing files are fine; broken ones are skipped
    /// with a warning (the daemon keeps running on the rest).
    pub fn load(paths: &Paths) -> (Config, Vec<String>) {
        let mut cfg = Config::default();
        let mut warnings = Vec::new();
        for (layer, path) in paths.all() {
            match std::fs::read_to_string(path) {
                Ok(text) => match text.parse::<Table>() {
                    Ok(t) => cfg.layers.push((layer, t)),
                    Err(e) => warnings.push(format!("{}: {e}", path.display())),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warnings.push(format!("{}: {e}", path.display())),
            }
        }
        (cfg, warnings)
    }

    /// From TOML texts, lowest precedence first (tests, previews).
    pub fn from_layers(layers: &[(Layer, &str)]) -> Result<Config, String> {
        let mut cfg = Config::default();
        for (l, text) in layers {
            cfg.layers
                .push((*l, text.parse::<Table>().map_err(|e| e.to_string())?));
        }
        Ok(cfg)
    }

    /// The raw value for `key` and the layer it came from. With `scene`,
    /// `scenes.<scene>.<key>` in a layer beats `<key>` in the same layer.
    pub fn raw(&self, key: &str, scene: Option<&str>) -> Option<(&Value, Layer)> {
        for (layer, table) in self.layers.iter().rev() {
            if let Some(id) = scene
                && let Some(v) = lookup(table, &format!("scenes.{id}.{key}"))
            {
                return Some((v, *layer));
            }
            if let Some(v) = lookup(table, key) {
                return Some((v, *layer));
            }
        }
        None
    }

    /// The effective value of `def`: the highest layer that has a valid one,
    /// else the default. Global options also take per-scene overrides.
    pub fn value(&self, def: &OptDef, scene: Option<&str>) -> (Value, Option<Layer>) {
        let scene = if def.key.starts_with("scenes.") {
            None
        } else {
            scene
        };
        for (layer, table) in self.layers.iter().rev() {
            let candidates = scene
                .map(|id| format!("scenes.{id}.{}", def.key))
                .into_iter()
                .chain([def.key.clone()]);
            for key in candidates {
                if let Some(v) = lookup(table, &key)
                    && let Ok(v) = def.coerce(v)
                {
                    return (v, Some(*layer));
                }
            }
        }
        (def.default.clone(), None)
    }

    /// Problems worth a warning: unknown keys and values that don't fit.
    pub fn check(&self, schema: &[OptDef]) -> Vec<String> {
        let mut out = Vec::new();
        for (layer, table) in &self.layers {
            let mut keys = Vec::new();
            flatten("", table, &mut keys);
            for (key, v) in keys {
                let def = schema.iter().find(|d| d.key == key).or_else(|| {
                    // scenes.<id>.<global key>
                    let rest = key.strip_prefix("scenes.")?.split_once('.')?.1;
                    schema.iter().find(|d| d.key == rest)
                });
                match def {
                    Some(d) => {
                        if let Err(e) = d.coerce(v) {
                            out.push(format!("{layer:?} config: `{key}`: {e}"));
                        }
                    }
                    // Params of scenes that aren't loaded can't be checked.
                    None if key.starts_with("scenes.") => {}
                    None => out.push(format!("{layer:?} config: unknown option `{key}`")),
                }
            }
        }
        out
    }
}

fn lookup<'a>(table: &'a Table, key: &str) -> Option<&'a Value> {
    let mut parts = key.split('.');
    let mut cur = table.get(parts.next()?)?;
    for p in parts {
        cur = cur.as_table()?.get(p)?;
    }
    Some(cur)
}

fn flatten<'a>(prefix: &str, t: &'a Table, out: &mut Vec<(String, &'a Value)>) {
    for (k, v) in t {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            Value::Table(sub) => flatten(&key, sub, out),
            _ => out.push((key, v)),
        }
    }
}

/// One line of `ukiyo config schema --json`.
#[derive(Debug, Clone, Serialize)]
pub struct SchemaEntry {
    #[serde(flatten)]
    pub def: OptDef,
    #[serde(serialize_with = "ser_value")]
    pub value: Value,
    /// Which file set it; null when it's the default.
    pub source: Option<Layer>,
}

pub fn schema_entries(cfg: &Config, schema: &[OptDef], scene: Option<&str>) -> Vec<SchemaEntry> {
    schema
        .iter()
        .map(|d| {
            let (value, source) = cfg.value(d, scene);
            SchemaEntry {
                def: d.clone(),
                value,
                source,
            }
        })
        .collect()
}

/// Edits to the user's config file, keeping its comments and layout.
pub mod edit {
    use std::path::Path;

    use toml::Value;
    use toml_edit::{DocumentMut, Item, Table};

    fn read(path: &Path) -> Result<DocumentMut, String> {
        match std::fs::read_to_string(path) {
            Ok(t) => t
                .parse::<DocumentMut>()
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    fn write(path: &Path, doc: &DocumentMut) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        // Atomic: write next to it, then rename over.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, doc.to_string()).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn to_edit(v: &Value) -> toml_edit::Value {
        match v {
            Value::String(s) => s.as_str().into(),
            Value::Integer(i) => (*i).into(),
            Value::Float(f) => (*f).into(),
            Value::Boolean(b) => (*b).into(),
            Value::Array(a) => {
                let mut arr = toml_edit::Array::new();
                for x in a {
                    arr.push(to_edit(x));
                }
                toml_edit::Value::Array(arr)
            }
            other => other.to_string().as_str().into(),
        }
    }

    /// Set `key` (dotted) to `value` in the file at `path`.
    pub fn set(path: &Path, key: &str, value: &Value) -> Result<(), String> {
        let mut doc = read(path)?;
        let parts: Vec<&str> = key.split('.').collect();
        let (last, tables) = parts.split_last().ok_or("empty key")?;
        let mut cur: &mut Table = doc.as_table_mut();
        for p in tables {
            let entry = cur.entry(p).or_insert_with(|| {
                let mut t = Table::new();
                t.set_implicit(true);
                Item::Table(t)
            });
            cur = entry
                .as_table_mut()
                .ok_or_else(|| format!("`{p}` in `{key}` isn't a table in {}", path.display()))?;
        }
        // Keep the line's comments when replacing a value.
        match cur.get_mut(last).and_then(Item::as_value_mut) {
            Some(existing) => {
                let decor = existing.decor().clone();
                let mut v = to_edit(value);
                *v.decor_mut() = decor;
                *existing = v;
            }
            None => {
                cur.insert(last, Item::Value(to_edit(value)));
            }
        }
        write(path, &doc)
    }

    /// Remove `key` (or a whole table, e.g. `scenes.<id>`). Empty parent
    /// tables go too. Returns whether anything was removed.
    pub fn reset(path: &Path, key: &str) -> Result<bool, String> {
        let mut doc = read(path)?;
        let parts: Vec<&str> = key.split('.').collect();
        let removed = remove(doc.as_table_mut(), &parts);
        if removed {
            write(path, &doc)?;
        }
        Ok(removed)
    }

    fn remove(t: &mut Table, parts: &[&str]) -> bool {
        match parts {
            [] => false,
            [last] => t.remove(last).is_some(),
            [first, rest @ ..] => {
                let Some(sub) = t.get_mut(first).and_then(Item::as_table_mut) else {
                    return false;
                };
                let removed = remove(sub, rest);
                if removed && sub.is_empty() {
                    t.remove(first);
                }
                removed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(key: &str) -> OptDef {
        global_schema().into_iter().find(|d| d.key == key).unwrap()
    }

    #[test]
    fn schema_keys_are_unique_and_defaults_valid() {
        let s = global_schema();
        for (i, d) in s.iter().enumerate() {
            assert!(s[..i].iter().all(|o| o.key != d.key), "duplicate {}", d.key);
            assert_eq!(d.coerce(&d.default).as_ref(), Ok(&d.default), "{}", d.key);
        }
        for k in [
            "signals.cpu.window",
            "signals.audio.device",
            "signals.typing.source",
            "signals.build.commands",
            "performance.on_battery",
        ] {
            assert!(s.iter().any(|d| d.key == k), "{k}");
        }
    }

    #[test]
    fn precedence_user_over_defaults_over_system_and_scene_over_global() {
        let cfg = Config::from_layers(&[
            (
                Layer::System,
                "intensity = 0.2\n[signals.cpu]\ngain = 0.5\n",
            ),
            (
                Layer::Defaults,
                "intensity = 0.4\n[scenes.gate]\nintensity = 0.6\n",
            ),
            (Layer::User, "[signals.cpu]\ngain = 2\n"),
        ])
        .unwrap();
        let i = def("intensity");
        assert_eq!(
            cfg.value(&i, None),
            (Value::Float(0.4), Some(Layer::Defaults))
        );
        assert_eq!(
            cfg.value(&i, Some("gate")),
            (Value::Float(0.6), Some(Layer::Defaults)),
            "scene override in the same layer"
        );
        assert_eq!(
            cfg.value(&i, Some("other")),
            (Value::Float(0.4), Some(Layer::Defaults))
        );
        assert_eq!(
            cfg.value(&def("signals.cpu.gain"), Some("gate")),
            (Value::Float(2.0), Some(Layer::User)),
            "integer -> float"
        );
        assert_eq!(
            cfg.value(&def("performance.fps"), None),
            (Value::Float(60.0), None)
        );

        // A user global value beats a defaults-file scene override.
        let cfg = Config::from_layers(&[
            (Layer::Defaults, "[scenes.gate]\nintensity = 0.6\n"),
            (Layer::User, "intensity = 1.5\n"),
        ])
        .unwrap();
        assert_eq!(cfg.value(&i, Some("gate")).0, Value::Float(1.5));
    }

    #[test]
    fn invalid_values_fall_through_and_are_reported() {
        let cfg = Config::from_layers(&[
            (Layer::Defaults, "performance.on_battery = \"still\"\n"),
            (
                Layer::User,
                "performance.on_battery = \"sometimes\"\nperformance.fps = 1000\nbogus = 1\n",
            ),
        ])
        .unwrap();
        assert_eq!(
            cfg.value(&def("performance.on_battery"), None).0,
            Value::String("still".into())
        );
        assert_eq!(
            cfg.value(&def("performance.fps"), None).0,
            Value::Float(144.0),
            "clamped"
        );
        let warnings = cfg.check(&global_schema());
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("on_battery") && w.contains("one of")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("unknown option `bogus`")),
            "{warnings:?}"
        );
    }

    #[test]
    fn cli_parsing() {
        assert_eq!(
            def("performance.motion").parse_cli("off"),
            Ok(Value::Boolean(false))
        );
        assert_eq!(def("intensity").parse_cli("0.5"), Ok(Value::Float(0.5)));
        assert!(def("intensity").parse_cli("lots").is_err());
        assert_eq!(
            def("signals.build.commands").parse_cli("cargo, make"),
            Ok(Value::Array(vec![s("cargo"), s("make")]))
        );
        assert!(def("clock.mode").parse_cli("sometimes").is_err());
    }

    #[test]
    fn scene_params_in_schema() {
        let m = Manifest::parse(
            "[scene]\nname = \"Gate\"\n[params.rain]\ntype = \"float\"\nlabel = \"Rain\"\ngroup = \"Weather\"\ndefault = 0.5\nmin = 0\nmax = 2\n",
        )
        .unwrap();
        let sch = schema(Some(("gate", &m)));
        let rain = sch.iter().find(|d| d.key == "scenes.gate.rain").unwrap();
        assert_eq!(rain.group, "Gate: Weather");
        assert_eq!(
            rain.kind,
            Kind::Float {
                min: 0.0,
                max: 2.0,
                step: 0.02,
                unit: String::new()
            }
        );
        let cfg = Config::from_layers(&[(Layer::User, "[scenes.gate]\nrain = 1.5\n")]).unwrap();
        let entries = schema_entries(&cfg, &sch, Some("gate"));
        let json = serde_json::to_value(&entries).unwrap();
        let rain = json
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["key"] == "scenes.gate.rain")
            .unwrap();
        assert_eq!(rain["type"], "float");
        assert_eq!(rain["min"], 0.0);
        assert_eq!(rain["default"], 0.5);
        assert_eq!(rain["value"], 1.5);
        assert_eq!(rain["source"], "user");
        assert_eq!(rain["label"], "Rain");
        let fps = json
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["key"] == "performance.fps")
            .unwrap();
        assert_eq!(fps["source"], serde_json::Value::Null);
        assert_eq!(fps["unit"], "fps");
        assert!(cfg.check(&sch).is_empty());
    }

    #[test]
    fn edit_set_and_reset_keep_comments() {
        let dir = std::env::temp_dir().join(format!("ukiyo-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "# mine\nintensity = 0.5 # calm\n").unwrap();
        edit::set(&path, "signals.cpu.gain", &Value::Float(1.5)).unwrap();
        edit::set(&path, "scenes.kabukicho-gate.rain", &Value::Float(0.9)).unwrap();
        edit::set(&path, "intensity", &Value::Float(0.7)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# mine"), "{text}");
        let cfg = Config::load(&Paths::in_dir(&dir)).0;
        assert_eq!(
            cfg.raw("signals.cpu.gain", None).unwrap().0,
            &Value::Float(1.5)
        );
        assert_eq!(
            cfg.raw("rain", Some("kabukicho-gate")).unwrap().0,
            &Value::Float(0.9)
        );
        assert_eq!(cfg.value(&def("intensity"), None).0, Value::Float(0.7));
        assert!(
            edit::reset(&path, "scenes.kabukicho-gate").unwrap(),
            "a whole scene"
        );
        assert!(!edit::reset(&path, "scenes.kabukicho-gate.rain").unwrap());
        assert!(edit::reset(&path, "signals.cpu.gain").unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("scenes") && !text.contains("signals"),
            "{text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_file_is_a_warning_not_an_error() {
        let dir = std::env::temp_dir().join(format!("ukiyo-cfg-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "intensity = [").unwrap();
        std::fs::write(dir.join("defaults.toml"), "intensity = 0.3").unwrap();
        let (cfg, warnings) = Config::load(&Paths::in_dir(&dir));
        assert_eq!(warnings.len(), 1);
        assert_eq!(cfg.value(&def("intensity"), None).0, Value::Float(0.3));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
