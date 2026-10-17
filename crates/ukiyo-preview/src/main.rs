//! ukiyo-preview: develop and tune scenes on any desktop (Metal on macOS,
//! Vulkan/GL elsewhere), fake every signal, and record scripted demos.
//!
//! ```sh
//! ukiyo-preview scenes/kabukicho-gate                       # window with sliders
//! ukiyo-preview scenes/kabukicho-gate --shot a.png --at 3 --signal typing.rate=1 --hour 3
//! ukiyo-preview scenes/kabukicho-gate --script demo.toml --record demo.mp4 --hud
//! ```

mod host;
mod hud;
mod offline;
mod script;
mod window;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use ukiyo_core::config::{self, Config, Layer};
use ukiyo_core::scene::Scene;

#[derive(Parser, Debug)]
#[command(version, about = "Preview, tune and record ukiyo scenes")]
pub struct Args {
    /// Scene directory (holding scene.toml).
    pub scene: PathBuf,
    /// Config files to layer (lowest first). Default: none, built-in defaults.
    #[arg(long = "config", value_name = "FILE")]
    pub configs: Vec<PathBuf>,
    /// Use the user's own config files (XDG) as the base.
    #[arg(long)]
    pub user_config: bool,
    /// Config override, e.g. `--set intensity=0.5 --set scenes.kabukicho-gate.rain=1`.
    #[arg(long = "set", value_name = "KEY=VALUE")]
    pub sets: Vec<String>,
    /// Raw signal value, e.g. `--signal typing.rate=1`.
    #[arg(long = "signal", value_name = "NAME=VALUE")]
    pub signals: Vec<String>,
    /// Fire a pulse at the start (shots: at `--at` minus 0.5 s).
    #[arg(long = "pulse", value_name = "NAME")]
    pub pulses: Vec<String>,
    /// Shots: how long before `--at` the pulses fire, seconds.
    #[arg(long, default_value_t = 0.5)]
    pub pulse_before: f64,
    /// Pin the time of day (0..24).
    #[arg(long)]
    pub hour: Option<f32>,
    /// Start still (as if locked).
    #[arg(long)]
    pub still: bool,
    /// Output size, WxH (window: initial size).
    #[arg(long, value_parser = parse_size)]
    pub size: Option<(u32, u32)>,
    /// Internal render scale override.
    #[arg(long)]
    pub scale: Option<f32>,
    /// Render one frame to this PNG and exit.
    #[arg(long, value_name = "PNG")]
    pub shot: Option<PathBuf>,
    /// Scene time of the shot, seconds (signals are simulated up to it).
    #[arg(long, default_value_t = 4.0)]
    pub at: f64,
    /// Timeline script (TOML), see script.rs.
    #[arg(long)]
    pub script: Option<PathBuf>,
    /// Record the script to a video through ffmpeg.
    #[arg(long, value_name = "MP4")]
    pub record: Option<PathBuf>,
    /// Also write PNG frames of the script, every N seconds, into this dir.
    #[arg(long, value_name = "DIR")]
    pub frames: Option<PathBuf>,
    #[arg(long, default_value_t = 1.0)]
    pub every: f64,
    /// Render this many frames offscreen (no readback) and print the time per frame.
    #[arg(long, value_name = "FRAMES")]
    pub bench: Option<u32>,
    /// Draw captions and signal meters into recordings.
    #[arg(long)]
    pub hud: bool,
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or("expected WxH")?;
    Ok((
        w.parse().map_err(|_| "bad width")?,
        h.parse().map_err(|_| "bad height")?,
    ))
}

/// The config: files, then `--set` overrides (as a user layer).
pub fn build_config(args: &Args, scene: &Scene) -> Result<Config> {
    let mut texts: Vec<(Layer, String)> = Vec::new();
    if args.user_config {
        let p = config::Paths::from_env();
        for path in p.system.iter().chain([&p.defaults, &p.user]) {
            if let Ok(t) = std::fs::read_to_string(path) {
                texts.push((Layer::Defaults, t));
            }
        }
    }
    for path in &args.configs {
        texts.push((
            Layer::Defaults,
            std::fs::read_to_string(path).with_context(|| path.display().to_string())?,
        ));
    }
    let schema = config::schema(Some((&scene.id, &scene.manifest)));
    let mut sets = String::new();
    for s in &args.sets {
        let (k, v) = s.split_once('=').context("--set wants KEY=VALUE")?;
        let def = schema
            .iter()
            .find(|d| d.key == k)
            .or_else(|| {
                let rest = k.strip_prefix("scenes.")?.split_once('.')?.1;
                schema.iter().find(|d| d.key == rest)
            })
            .with_context(|| format!("unknown option `{k}`"))?;
        let v = def.parse_cli(v).map_err(|e| anyhow::anyhow!("{k}: {e}"))?;
        let key = k
            .split('.')
            .map(|p| format!("\"{p}\""))
            .collect::<Vec<_>>()
            .join(".");
        sets += &format!("{key} = {v}\n");
    }
    texts.push((Layer::User, sets));
    let refs: Vec<(Layer, &str)> = texts.iter().map(|(l, t)| (*l, t.as_str())).collect();
    Config::from_layers(&refs).map_err(|e| anyhow::anyhow!(e))
}

pub fn parse_signals(args: &Args) -> Result<Vec<(String, f32)>> {
    args.signals
        .iter()
        .map(|s| {
            let (k, v) = s.split_once('=').context("--signal wants NAME=VALUE")?;
            Ok((
                k.to_string(),
                v.parse()
                    .with_context(|| format!("--signal {k}: not a number"))?,
            ))
        })
        .collect()
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,ukiyo=info"))
        .init();
    let args = Args::parse();
    if args.record.is_some() && args.script.is_none() {
        bail!("--record needs a --script");
    }
    if args.shot.is_some() || args.script.is_some() || args.bench.is_some() {
        return offline::run(&args);
    }
    window::run(args)
}
