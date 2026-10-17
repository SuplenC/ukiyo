//! The scene format: a directory with `scene.toml`, a WGSL shader and
//! textures. See `docs/SCENE-FORMAT.md` for the author's guide.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::expr::{Expr, ParseError};
use crate::smooth::Smoothing;

/// Names a scene can't use for a binding: they are built-in uniform fields.
pub const RESERVED: &[&str] = &[
    "resolution",
    "cover_scale",
    "cover_center",
    "time",
    "dt",
    "still",
    "frame",
    "seed",
    "wall",
];

#[derive(Debug, thiserror::Error)]
pub enum SceneError {
    #[error("can't read {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: {message}")]
    Toml { path: PathBuf, message: String },
    #[error("binding `{name}`: {error} in `{expr}`")]
    Expr {
        name: String,
        expr: String,
        error: ParseError,
    },
    #[error("{0}")]
    Invalid(String),
}

/// `scene.toml`, as written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub scene: Meta,
    #[serde(default)]
    pub view: View,
    #[serde(default)]
    pub render: Render,
    #[serde(default)]
    pub textures: BTreeMap<String, TextureSpec>,
    /// Tunables: user-facing knobs with a type, range, default and label.
    /// Each is a uniform field and reads as `param.<name>` in expressions.
    #[serde(default, with = "ordered")]
    pub params: Vec<(String, ParamSpec)>,
    /// Kept in file order: later bindings may read earlier ones.
    #[serde(default, with = "ordered")]
    pub bindings: Vec<(String, BindingSpec)>,
}

/// A scene tunable, declared in `[params.<name>]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamSpec {
    #[serde(rename = "type")]
    pub kind: ParamKind,
    pub label: String,
    #[serde(default)]
    pub help: String,
    #[serde(default)]
    pub group: String,
    pub default: toml::Value,
    #[serde(default)]
    pub min: Option<f32>,
    #[serde(default)]
    pub max: Option<f32>,
    #[serde(default)]
    pub step: Option<f32>,
    /// `choice` only: the labels; the value in the shader is the index.
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamKind {
    Float,
    Bool,
    Choice,
    /// `"#rrggbb"`, a `vec4<f32>` (linear rgb, a = 1) in the shader.
    Color,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// SPDX id of the scene's assets (the code is the engine's).
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub credits: Vec<String>,
    /// A still for pickers, colour extraction and the lock screen.
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default = "default_shader")]
    pub shader: String,
}

fn default_shader() -> String {
    "scene.wgsl".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    /// The texture whose aspect ratio the scene is framed by (default: the
    /// first texture, alphabetically).
    #[serde(default)]
    pub frame: Option<String>,
    /// Centre of the cover crop, in image uv (0..1).
    #[serde(default = "default_focus")]
    pub focus: [f32; 2],
    /// Extra zoom, so camera moves never show an edge.
    #[serde(default = "default_overscan")]
    pub overscan: f32,
}

fn default_focus() -> [f32; 2] {
    [0.5, 0.5]
}

fn default_overscan() -> f32 {
    1.0
}

impl Default for View {
    fn default() -> Self {
        View {
            frame: None,
            focus: default_focus(),
            overscan: default_overscan(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Render {
    /// Frame rate cap while the scene moves.
    #[serde(default = "default_fps")]
    pub fps: f32,
    /// Internal resolution of the scene pass, as a fraction of the output.
    #[serde(default = "default_scale")]
    pub scale: f32,
    /// The scene moves on its own (rain, flicker). When false, frames are
    /// only drawn when a binding changes.
    #[serde(default = "default_true")]
    pub animated: bool,
    #[serde(default)]
    pub bloom: Bloom,
}

fn default_fps() -> f32 {
    30.0
}

fn default_scale() -> f32 {
    1.0
}

fn default_true() -> bool {
    true
}

impl Default for Render {
    fn default() -> Self {
        Render {
            fps: default_fps(),
            scale: default_scale(),
            animated: true,
            bloom: Bloom::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bloom {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Brightness (in the scene pass's linear HDR output) where bloom starts.
    #[serde(default = "default_threshold")]
    pub threshold: f32,
    /// Soft knee width around the threshold.
    #[serde(default = "default_knee")]
    pub knee: f32,
    /// Number of downsampled levels: more is wider.
    #[serde(default = "default_levels")]
    pub levels: u32,
}

fn default_threshold() -> f32 {
    1.0
}

fn default_knee() -> f32 {
    0.5
}

fn default_levels() -> u32 {
    6
}

impl Default for Bloom {
    fn default() -> Self {
        Bloom {
            enabled: true,
            threshold: default_threshold(),
            knee: default_knee(),
            levels: default_levels(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorSpace {
    /// Colour photos: decoded to linear by the sampler.
    Srgb,
    /// Data: depth, masks.
    #[default]
    Linear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Wrap {
    #[default]
    Clamp,
    Repeat,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TextureSpec {
    Path(String),
    Full(TextureFull),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextureFull {
    pub path: String,
    #[serde(default)]
    pub color: ColorSpace,
    #[serde(default = "default_true")]
    pub mipmaps: bool,
    #[serde(default)]
    pub wrap: Wrap,
}

impl TextureSpec {
    pub fn full(&self) -> TextureFull {
        match self {
            TextureSpec::Path(p) => TextureFull {
                path: p.clone(),
                color: ColorSpace::Linear,
                mipmaps: true,
                wrap: Wrap::Clamp,
            },
            TextureSpec::Full(f) => f.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BindingSpec {
    Expr(String),
    Full(BindingFull),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingFull {
    pub expr: String,
    /// Symmetric smoothing time constant, seconds. `attack` and `release`
    /// override it per direction.
    #[serde(default)]
    pub smooth: Option<f32>,
    #[serde(default)]
    pub attack: Option<f32>,
    #[serde(default)]
    pub release: Option<f32>,
    #[serde(default)]
    pub min: Option<f32>,
    #[serde(default)]
    pub max: Option<f32>,
}

impl BindingSpec {
    pub fn expr(&self) -> &str {
        match self {
            BindingSpec::Expr(e) => e,
            BindingSpec::Full(f) => &f.expr,
        }
    }

    pub fn smoothing(&self) -> Smoothing {
        match self {
            BindingSpec::Expr(_) => Smoothing::NONE,
            BindingSpec::Full(f) => {
                let s = f.smooth.unwrap_or(0.0);
                Smoothing {
                    attack: f.attack.unwrap_or(s).max(0.0),
                    release: f.release.unwrap_or(s).max(0.0),
                }
            }
        }
    }

    pub fn range(&self) -> (f32, f32) {
        match self {
            BindingSpec::Expr(_) => (f32::NEG_INFINITY, f32::INFINITY),
            BindingSpec::Full(f) => (
                f.min.unwrap_or(f32::NEG_INFINITY),
                f.max.unwrap_or(f32::INFINITY),
            ),
        }
    }
}

/// A scene loaded from disk: the manifest, where it lives and its shader.
#[derive(Debug, Clone)]
pub struct Scene {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub shader_source: String,
}

impl Scene {
    /// Load `dir/scene.toml` and the shader it names. Textures load later,
    /// on the GPU side.
    pub fn load(dir: impl AsRef<Path>) -> Result<Scene, SceneError> {
        let dir = dir.as_ref().to_path_buf();
        let toml_path = dir.join("scene.toml");
        let text = std::fs::read_to_string(&toml_path).map_err(|source| SceneError::Io {
            path: toml_path.clone(),
            source,
        })?;
        let manifest = Manifest::parse(&text).map_err(|e| match e {
            SceneError::Toml { message, .. } => SceneError::Toml {
                path: toml_path.clone(),
                message,
            },
            e => e,
        })?;
        let shader_path = dir.join(&manifest.scene.shader);
        let shader_source =
            std::fs::read_to_string(&shader_path).map_err(|source| SceneError::Io {
                path: shader_path,
                source,
            })?;
        let id = manifest.scene.id.clone().unwrap_or_else(|| {
            dir.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "scene".into())
        });
        Ok(Scene {
            id,
            dir,
            manifest,
            shader_source,
        })
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    pub fn poster_path(&self) -> Option<PathBuf> {
        self.manifest.scene.poster.as_deref().map(|p| self.path(p))
    }
}

impl Manifest {
    /// Parse and validate `scene.toml` text.
    pub fn parse(text: &str) -> Result<Manifest, SceneError> {
        let m: Manifest = toml::from_str(text).map_err(|e| SceneError::Toml {
            path: PathBuf::from("scene.toml"),
            message: e.to_string(),
        })?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), SceneError> {
        let r = &self.render;
        if !(r.fps > 0.0 && r.fps <= 240.0) {
            return Err(SceneError::Invalid(format!(
                "render.fps must be in (0, 240], got {}",
                r.fps
            )));
        }
        if !(r.scale > 0.05 && r.scale <= 2.0) {
            return Err(SceneError::Invalid(format!(
                "render.scale must be in (0.05, 2], got {}",
                r.scale
            )));
        }
        if !(1..=10).contains(&r.bloom.levels) {
            return Err(SceneError::Invalid(format!(
                "render.bloom.levels must be 1..=10, got {}",
                r.bloom.levels
            )));
        }
        if !(self.view.overscan >= 1.0 && self.view.overscan <= 2.0) {
            return Err(SceneError::Invalid(format!(
                "view.overscan must be in [1, 2], got {}",
                self.view.overscan
            )));
        }
        for name in self.textures.keys() {
            if !is_ident(name) {
                return Err(SceneError::Invalid(format!(
                    "texture name `{name}` must be a WGSL identifier (a-z, 0-9, _)"
                )));
            }
        }
        if let Some(frame) = &self.view.frame
            && !self.textures.contains_key(frame)
        {
            return Err(SceneError::Invalid(format!(
                "view.frame names texture `{frame}`, which isn't in [textures]"
            )));
        }
        let mut seen: Vec<&str> = Vec::new();
        for (name, p) in &self.params {
            if !is_ident(name) {
                return Err(SceneError::Invalid(format!(
                    "param name `{name}` must be a WGSL identifier (a-z, 0-9, _)"
                )));
            }
            if RESERVED.contains(&name.as_str()) || name.starts_with("_pad") {
                return Err(SceneError::Invalid(format!(
                    "param name `{name}` is reserved (built-in uniform)"
                )));
            }
            if seen.contains(&name.as_str()) {
                return Err(SceneError::Invalid(format!(
                    "param `{name}` is defined twice"
                )));
            }
            seen.push(name);
            p.check(name)?;
        }
        for (name, spec) in &self.bindings {
            if !is_ident(name) {
                return Err(SceneError::Invalid(format!(
                    "binding name `{name}` must be a WGSL identifier (a-z, 0-9, _)"
                )));
            }
            if RESERVED.contains(&name.as_str()) || name.starts_with("_pad") {
                return Err(SceneError::Invalid(format!(
                    "binding name `{name}` is reserved (built-in uniform)"
                )));
            }
            if seen.contains(&name.as_str()) {
                return Err(SceneError::Invalid(format!(
                    "binding `{name}` is defined twice, or shadows a param"
                )));
            }
            seen.push(name);
            Expr::parse(spec.expr()).map_err(|error| SceneError::Expr {
                name: name.clone(),
                expr: spec.expr().to_string(),
                error,
            })?;
        }
        Ok(())
    }

    /// The texture that frames the scene.
    pub fn frame_texture(&self) -> Option<&str> {
        self.view
            .frame
            .as_deref()
            .or_else(|| self.textures.keys().next().map(String::as_str))
    }
}

impl ParamSpec {
    fn check(&self, name: &str) -> Result<(), SceneError> {
        let bad = |m: String| Err(SceneError::Invalid(format!("param `{name}`: {m}")));
        match self.kind {
            ParamKind::Float => {
                let Some(v) = value_f32(&self.default) else {
                    return bad("default must be a number".into());
                };
                let (lo, hi) = (
                    self.min.unwrap_or(f32::NEG_INFINITY),
                    self.max.unwrap_or(f32::INFINITY),
                );
                if lo > hi || !(lo..=hi).contains(&v) {
                    return bad(format!("default {v} is outside [{lo}, {hi}]"));
                }
            }
            ParamKind::Bool => {
                if self.default.as_bool().is_none() {
                    return bad("default must be true or false".into());
                }
            }
            ParamKind::Choice => {
                if self.options.is_empty() {
                    return bad("a choice needs `options`".into());
                }
                match self.default.as_str() {
                    Some(d) if self.options.iter().any(|o| o == d) => {}
                    _ => return bad("default must be one of `options`".into()),
                }
            }
            ParamKind::Color => {
                if self.default.as_str().and_then(parse_color).is_none() {
                    return bad("default must be a \"#rrggbb\" colour".into());
                }
            }
        }
        Ok(())
    }

    /// The uniform value(s) for a config value of this param, or None if
    /// the value doesn't fit (wrong type, out of range: clamped instead).
    pub fn uniform(&self, v: &toml::Value) -> Option<[f32; 4]> {
        match self.kind {
            ParamKind::Float => {
                let x = value_f32(v)?;
                let x = x.clamp(
                    self.min.unwrap_or(f32::NEG_INFINITY),
                    self.max.unwrap_or(f32::INFINITY),
                );
                Some([x, 0.0, 0.0, 0.0])
            }
            ParamKind::Bool => Some([if v.as_bool()? { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0]),
            ParamKind::Choice => {
                let i = self
                    .options
                    .iter()
                    .position(|o| Some(o.as_str()) == v.as_str())?;
                Some([i as f32, 0.0, 0.0, 0.0])
            }
            ParamKind::Color => {
                let [r, g, b] = parse_color(v.as_str()?)?;
                Some([srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), 1.0])
            }
        }
    }
}

/// A TOML number (integer or float) as f32.
pub fn value_f32(v: &toml::Value) -> Option<f32> {
    match v {
        toml::Value::Float(f) => Some(*f as f32),
        toml::Value::Integer(i) => Some(*i as f32),
        _ => None,
    }
}

/// `#rrggbb` (or `rrggbb`) to sRGB components in 0..1.
pub fn parse_color(s: &str) -> Option<[f32; 3]> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let c = |i: usize| {
        u8::from_str_radix(&h[i..i + 2], 16)
            .ok()
            .map(|v| v as f32 / 255.0)
    };
    Some([c(0)?, c(2)?, c(4)?])
}

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && s != "_"
}

/// Serde for `[bindings]` that keeps the table's order.
mod ordered {
    use std::fmt;

    use serde::de::{MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};

    use serde::{Deserialize, Serialize};

    pub fn serialize<S: Serializer, T: Serialize>(
        v: &[(String, T)],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(v.len()))?;
        for (k, b) in v {
            map.serialize_entry(k, b)?;
        }
        map.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Vec<(String, T)>, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> Visitor<'de> for V<T> {
            type Value = Vec<(String, T)>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a table")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, T>()? {
                    out.push((k, v));
                }
                Ok(out)
            }
        }
        d.deserialize_map(V(std::marker::PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r##"
[scene]
name = "Test"
author = "me"
license = "CC0-1.0"
poster = "poster.jpg"

[view]
focus = [0.5, 0.3]
overscan = 1.05

[render]
fps = 24
scale = 0.75
bloom = { threshold = 0.8, levels = 5 }

[textures]
base = { path = "base.jpg", color = "srgb" }
depth = "depth.png"

[params.rain]
type = "float"
label = "Rain"
group = "Weather"
default = 0.5
min = 0
max = 1

[params.tint]
type = "color"
label = "Tint"
default = "#ff8000"

[params.mode]
type = "choice"
label = "Mode"
options = ["calm", "storm"]
default = "storm"

[bindings]
zeta = "cpu.load"
flicker = { expr = "typing.rate * 0.8 + cpu.load * 0.3", attack = 0.05, release = 0.8 }
alpha = { expr = "flicker * 2", smooth = 0.5, max = 1 }
"##;

    #[test]
    fn parses_a_full_manifest() {
        let m = Manifest::parse(FULL).unwrap();
        assert_eq!(m.scene.name, "Test");
        assert_eq!(m.scene.shader, "scene.wgsl");
        assert_eq!(m.view.focus, [0.5, 0.3]);
        assert_eq!(m.render.fps, 24.0);
        assert_eq!(m.render.bloom.threshold, 0.8);
        assert_eq!(m.render.bloom.knee, 0.5, "defaults fill in");
        assert!(m.render.animated);
        assert_eq!(m.textures["base"].full().color, ColorSpace::Srgb);
        assert_eq!(m.textures["depth"].full().color, ColorSpace::Linear);
        assert_eq!(m.frame_texture(), Some("base"));
        let names: Vec<&str> = m.bindings.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["zeta", "flicker", "alpha"], "file order is kept");
        assert_eq!(
            m.bindings[1].1.smoothing(),
            Smoothing {
                attack: 0.05,
                release: 0.8
            }
        );
        assert_eq!(m.bindings[2].1.smoothing(), Smoothing::symmetric(0.5));
        assert_eq!(m.bindings[2].1.range(), (f32::NEG_INFINITY, 1.0));
        assert_eq!(m.bindings[0].1.smoothing(), Smoothing::NONE);
        let params: Vec<&str> = m.params.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(params, ["rain", "tint", "mode"]);
        let (_, rain) = &m.params[0];
        assert_eq!(
            rain.uniform(&toml::Value::Float(2.0)),
            Some([1.0, 0.0, 0.0, 0.0]),
            "clamped to max"
        );
        assert_eq!(
            rain.uniform(&toml::Value::Integer(0)),
            Some([0.0, 0.0, 0.0, 0.0])
        );
        assert_eq!(rain.uniform(&toml::Value::Boolean(true)), None);
        let (_, mode) = &m.params[2];
        assert_eq!(
            mode.uniform(&toml::Value::String("storm".into())),
            Some([1.0, 0.0, 0.0, 0.0])
        );
        let (_, tint) = &m.params[1];
        let c = tint
            .uniform(&toml::Value::String("#ffffff".into()))
            .unwrap();
        assert!((c[0] - 1.0).abs() < 1e-6 && c[3] == 1.0);
    }

    #[test]
    fn minimal_manifest_gets_defaults() {
        let m = Manifest::parse("[scene]\nname = \"x\"\n").unwrap();
        assert_eq!(m.render, Render::default());
        assert_eq!(m.view, View::default());
        assert!(m.bindings.is_empty());
        assert_eq!(m.frame_texture(), None);
    }

    #[test]
    fn rejects_bad_manifests() {
        let bad = [
            (
                "[scene]\nname = \"x\"\n[bindings]\ntime = \"1\"\n",
                "reserved",
            ),
            (
                "[scene]\nname = \"x\"\n[bindings]\nFoo = \"1\"\n",
                "identifier",
            ),
            (
                "[scene]\nname = \"x\"\n[bindings]\nfoo = \"1 +\"\n",
                "binding `foo`",
            ),
            ("[scene]\nname = \"x\"\n[render]\nfps = 0\n", "render.fps"),
            (
                "[scene]\nname = \"x\"\n[render]\nscale = 3\n",
                "render.scale",
            ),
            (
                "[scene]\nname = \"x\"\n[view]\nframe = \"nope\"\n",
                "view.frame",
            ),
            (
                "[scene]\nname = \"x\"\n[textures]\n\"bad-name\" = \"a.png\"\n",
                "texture name",
            ),
            ("[scene]\nname = \"x\"\nwhat = 1\n", "unknown field"),
            ("[scene]\n", "missing field `name`"),
            (
                "[scene]\nname = \"x\"\n[params.a]\ntype = \"float\"\nlabel = \"A\"\ndefault = 5\nmax = 1\n",
                "outside",
            ),
            (
                "[scene]\nname = \"x\"\n[params.a]\ntype = \"choice\"\nlabel = \"A\"\ndefault = \"z\"\noptions = [\"y\"]\n",
                "one of",
            ),
            (
                "[scene]\nname = \"x\"\n[params.a]\ntype = \"color\"\nlabel = \"A\"\ndefault = \"red\"\n",
                "colour",
            ),
            (
                "[scene]\nname = \"x\"\n[params.a]\ntype = \"bool\"\nlabel = \"A\"\ndefault = true\n[bindings]\na = \"1\"\n",
                "shadows",
            ),
        ];
        for (text, want) in bad {
            let err = Manifest::parse(text).unwrap_err().to_string();
            assert!(err.contains(want), "{text:?}: {err}");
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let m = Manifest::parse(FULL).unwrap();
        let text = toml::to_string(&m).unwrap();
        assert_eq!(Manifest::parse(&text).unwrap(), m);
    }
}
