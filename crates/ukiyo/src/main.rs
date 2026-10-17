//! `ukiyo`: the reactive wallpaper daemon (`ukiyo daemon`) and the CLI
//! that controls it.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

mod client;
mod confops;
#[cfg(target_os = "linux")]
mod daemon;
mod hook;
mod sources;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::{Value as Json, json};
use ukiyo_core::catalog;
use ukiyo_core::config::{Config, Paths};
use ukiyo_core::ipc::{Request, Response};
use ukiyo_core::signals;

use client::ClientError;

#[derive(Parser)]
#[command(
    name = "ukiyo",
    version,
    about = "Reactive live wallpapers for Wayland",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the wallpaper daemon (Linux, wlroots compositors).
    Daemon {
        /// Scene id or directory (default: config `scene`, else the first installed).
        #[arg(long)]
        scene: Option<String>,
    },
    /// Show a scene (an id from `ukiyo scenes`, or a directory).
    Set { scene: String },
    /// List installed scenes.
    Scenes {
        #[arg(long)]
        json: bool,
    },
    /// What the daemon is doing: scene, state, outputs, signals.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Write a signal (`cpu.load 0.5`). Without a value: fire a pulse
    /// (`build-failed` fires `pulse.build_failed`).
    Signal { name: String, value: Option<String> },
    /// Fire `pulse.<name>`.
    Pulse { name: String },
    /// Report a finished shell command (what the shell hook sends).
    Command {
        #[arg(long, allow_hyphen_values = true)]
        status: i32,
        #[arg(long)]
        duration: f32,
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Stop drawing (the last frame stays) until `resume`.
    Pause {
        #[arg(long, default_value = "user")]
        reason: String,
    },
    /// Undo `pause` (for that reason) and `still`.
    Resume {
        #[arg(long, default_value = "user")]
        reason: String,
    },
    /// Ease to a standstill, then stop drawing (for a lock screen).
    Still,
    /// Save every output's current frame as PNG.
    Snapshot {
        /// Directory (default: the current one).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Re-read the config files now.
    Reload,
    /// Stop the daemon.
    Quit,
    /// Read and change settings.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Print a shell snippet that reports finished commands (zsh, bash).
    ShellHook { shell: String },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// The effective value of an option.
    Get {
        key: String,
        #[arg(long)]
        json: bool,
    },
    /// Set an option in the user config (applies live).
    Set { key: String, value: String },
    /// Remove an option (or `scenes.<id>`: all of a scene's) from the user config.
    Reset { key: String },
    /// Every option with type, range, default, current value and label.
    Schema {
        #[arg(long)]
        json: bool,
        /// Include this scene's params (default: the current scene).
        #[arg(long)]
        scene: Option<String>,
    },
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .format_timestamp_millis()
        .init();
    let cli = Cli::parse();
    // `ukiyo status | head` should end quietly, like any Unix tool.
    #[cfg(unix)]
    if !matches!(cli.cmd, Cmd::Daemon { .. }) {
        // SAFETY: resetting a signal disposition at startup, before threads.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
    match run(cli.cmd) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ukiyo: {e}");
            ExitCode::FAILURE
        }
    }
}

type Res = Result<(), String>;

/// Send a request, fail on an error response, return its data.
fn call(req: Request) -> Result<Json, String> {
    let r: Response = client::send(&req).map_err(|e| e.to_string())?;
    if r.ok {
        Ok(r.data)
    } else {
        Err(r.error.unwrap_or_else(|| "the daemon said no".into()))
    }
}

fn print_json(v: &Json) {
    println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| "null".into())
    );
}

/// `ukiyo signal` value: a number, or on/off.
fn parse_signal_value(s: &str) -> Result<f32, String> {
    let v = match s.trim().to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" => 1.0,
        "off" | "false" | "no" => 0.0,
        t => t
            .parse::<f32>()
            .map_err(|_| format!("`{s}` isn't a number"))?,
    };
    if v.is_finite() {
        Ok(v)
    } else {
        Err(format!("`{s}` isn't a finite number"))
    }
}

/// The request `ukiyo signal <name> [value]` sends.
fn signal_request(name: &str, value: Option<&str>) -> Result<Request, String> {
    let name = signals::normalize(name);
    match value {
        None => {
            let base = name.strip_prefix("pulse.").unwrap_or(&name);
            if !signals::valid_name(&format!("pulse.{base}")) {
                return Err(format!("`{name}` isn't a valid pulse name"));
            }
            Ok(Request::Pulse { name: base.into() })
        }
        Some(v) => {
            if !signals::valid_name(&name) || !name.contains('.') {
                return Err(format!(
                    "`{name}` isn't a signal name (like `cpu.load` or `my.thing`)"
                ));
            }
            Ok(Request::Signal {
                name,
                value: parse_signal_value(v)?,
            })
        }
    }
}

fn run(cmd: Cmd) -> Res {
    match cmd {
        Cmd::Daemon { scene } => daemon_main(scene),
        Cmd::Set { scene } => {
            // A directory is resolved here: the daemon may run elsewhere.
            let scene = if scene.contains('/') {
                std::fs::canonicalize(&scene)
                    .map_err(|e| format!("{scene}: {e}"))?
                    .to_string_lossy()
                    .into_owned()
            } else {
                scene
            };
            let d = call(Request::Set { scene })?;
            println!(
                "{} ({})",
                d["name"].as_str().unwrap_or(""),
                d["id"].as_str().unwrap_or("")
            );
            Ok(())
        }
        Cmd::Scenes { json } => scenes(json),
        Cmd::Status { json } => {
            let d = call(Request::Status)?;
            if json {
                print_json(&d);
            } else {
                print_status(&d);
            }
            Ok(())
        }
        Cmd::Signal { name, value } => {
            call(signal_request(&name, value.as_deref())?)?;
            Ok(())
        }
        Cmd::Pulse { name } => {
            call(signal_request(&name, None)?)?;
            Ok(())
        }
        Cmd::Command {
            status,
            duration,
            command,
        } => {
            match client::send(&Request::Command {
                command: command.join(" "),
                status,
                duration,
            }) {
                // A shell hook shouldn't complain when no daemon runs.
                Err(ClientError::NotRunning(_)) => Ok(()),
                Err(e) => Err(e.to_string()),
                Ok(r) if !r.ok => Err(r.error.unwrap_or_default()),
                Ok(_) => Ok(()),
            }
        }
        Cmd::Pause { reason } => call(Request::Pause {
            reason: Some(reason),
        })
        .map(drop),
        Cmd::Resume { reason } => call(Request::Resume {
            reason: Some(reason),
        })
        .map(drop),
        Cmd::Still => call(Request::Still).map(drop),
        Cmd::Snapshot { dir } => {
            let dir = match dir {
                Some(d) => d,
                None => std::env::current_dir().map_err(|e| e.to_string())?,
            };
            let dir = std::path::absolute(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let d = call(Request::Snapshot { dir: Some(dir) })?;
            for p in d.as_array().into_iter().flatten() {
                println!("{}", p.as_str().unwrap_or(""));
            }
            Ok(())
        }
        Cmd::Reload => call(Request::Reload).map(drop),
        Cmd::Quit => call(Request::Quit).map(drop),
        Cmd::Config { cmd } => config_cmd(cmd),
        Cmd::ShellHook { shell } => {
            let s = hook::snippet(&shell)
                .ok_or_else(|| format!("no hook for `{shell}` (zsh and bash are supported)"))?;
            print!("{s}");
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
fn daemon_main(scene: Option<String>) -> Res {
    daemon::run(scene).map_err(|e| format!("{e:#}"))
}

#[cfg(not(target_os = "linux"))]
fn daemon_main(_scene: Option<String>) -> Res {
    Err("the daemon runs on Linux/Wayland only".into())
}

fn scenes(json: bool) -> Res {
    let list = catalog::list(&catalog::search_path());
    let current = Config::load(&Paths::from_env())
        .0
        .raw("scene", None)
        .and_then(|(v, _)| v.as_str().map(String::from));
    if json {
        let v: Vec<Json> = list
            .iter()
            .map(|e| {
                let mut j = serde_json::to_value(e).unwrap_or(Json::Null);
                j["current"] = json!(current.as_deref() == Some(e.id.as_str()));
                j
            })
            .collect();
        print_json(&Json::Array(v));
        return Ok(());
    }
    if list.is_empty() {
        return Err(format!(
            "no scenes installed (searched {})",
            catalog::search_path()
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for e in list {
        let mark = if current.as_deref() == Some(e.id.as_str()) {
            "*"
        } else {
            " "
        };
        println!("{mark} {:<24} {}", e.id, e.name);
    }
    Ok(())
}

fn print_status(d: &Json) {
    let s = &d["scene"];
    println!(
        "scene    {} ({})",
        s["name"].as_str().unwrap_or("-"),
        s["id"].as_str().unwrap_or("-")
    );
    let reasons: Vec<String> = d["reasons"]
        .as_array()
        .into_iter()
        .flatten()
        .map(confops::short)
        .collect();
    let state = d["state"].as_str().unwrap_or("-");
    if reasons.is_empty() {
        println!("state    {state}");
    } else {
        println!("state    {state} ({})", reasons.join(", "));
    }
    for o in d["outputs"].as_array().into_iter().flatten() {
        println!(
            "output   {}  {}x{} @{} -> {}x{}  {:.1} fps{}",
            o["name"].as_str().unwrap_or("?"),
            o["logical"][0],
            o["logical"][1],
            o["scale"],
            o["buffer"][0],
            o["buffer"][1],
            o["fps"].as_f64().unwrap_or(0.0),
            if o["covered"].as_bool() == Some(true) {
                "  (covered by fullscreen)"
            } else {
                ""
            }
        );
    }
    for p in d["still_frames"].as_array().into_iter().flatten() {
        println!("still    {}", p.as_str().unwrap_or(""));
    }
    if let Some(src) = d["sources"].as_object() {
        let v: Vec<String> = src
            .iter()
            .map(|(k, v)| format!("{k}={}", confops::short(v)))
            .collect();
        println!("sources  {}", v.join(" "));
    }
    if let Some(sig) = d["signals"].as_object() {
        let v: Vec<String> = sig
            .iter()
            .filter(|(k, _)| !k.ends_with(".age"))
            .map(|(k, v)| format!("{k}={:.2}", v["value"].as_f64().unwrap_or(0.0)))
            .collect();
        println!("signals  {}", v.join(" "));
    }
    if let Some(b) = d["bindings"].as_object() {
        let v: Vec<String> = b
            .iter()
            .map(|(k, v)| format!("{k}={:.2}", v.as_f64().unwrap_or(0.0)))
            .collect();
        println!("bindings {}", v.join(" "));
    }
}

/// The scene whose params `config` commands see when no daemon runs: the
/// configured one if it's installed.
fn offline_scene(
    cfg: &Config,
    wanted: Option<&str>,
) -> Result<Option<(String, ukiyo_core::scene::Manifest)>, String> {
    let id = match wanted {
        Some(id) => Some(id.to_string()),
        None => cfg
            .raw("scene", None)
            .and_then(|(v, _)| v.as_str().map(String::from))
            .filter(|s| !s.is_empty()),
    };
    match id {
        None => Ok(None),
        Some(id) => match confops::load_manifest(&id) {
            Ok(m) => Ok(Some((id, m))),
            Err(e) if wanted.is_some() => Err(e),
            Err(_) => Ok(None),
        },
    }
}

fn config_cmd(cmd: ConfigCmd) -> Res {
    // With a daemon: it validates, persists and applies at once.
    let daemon = client::running();
    let paths = Paths::from_env();
    match cmd {
        ConfigCmd::Get { key, json } => {
            let d = if daemon {
                call(Request::ConfigGet { key })?
            } else {
                let cfg = Config::load(&paths).0;
                let sc = offline_scene(&cfg, None)?;
                confops::get(&cfg, &key, sc.as_ref().map(|(i, m)| (i.as_str(), m)))?
            };
            if json {
                print_json(&d);
            } else {
                println!("{}", confops::short(&d["value"]));
            }
            Ok(())
        }
        ConfigCmd::Set { key, value } => {
            if daemon {
                call(Request::ConfigSet {
                    key,
                    value: Json::String(value),
                })?;
            } else {
                let cfg = Config::load(&paths).0;
                let sc = offline_scene(&cfg, None)?;
                confops::set(
                    &paths,
                    &key,
                    &Json::String(value),
                    sc.as_ref().map(|(i, m)| (i.as_str(), m)),
                )?;
            }
            Ok(())
        }
        ConfigCmd::Reset { key } => {
            if daemon {
                call(Request::ConfigReset { key })?;
            } else {
                let cfg = Config::load(&paths).0;
                let sc = offline_scene(&cfg, None)?;
                confops::reset(&paths, &key, sc.as_ref().map(|(i, m)| (i.as_str(), m)))?;
            }
            Ok(())
        }
        ConfigCmd::Schema { json, scene } => {
            let d = if daemon {
                call(Request::ConfigSchema { scene })?
            } else {
                let cfg = Config::load(&paths).0;
                let sc = offline_scene(&cfg, scene.as_deref())?;
                confops::schema_json(&cfg, sc.as_ref().map(|(i, m)| (i.as_str(), m)))
            };
            if json {
                print_json(&d);
            } else {
                let mut group = String::new();
                for e in d.as_array().into_iter().flatten() {
                    let g = e["group"].as_str().unwrap_or("");
                    if g != group {
                        println!("\n[{g}]");
                        group = g.to_string();
                    }
                    println!("{}", confops::describe(e));
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn signal_values() {
        assert_eq!(parse_signal_value("0.5"), Ok(0.5));
        assert_eq!(parse_signal_value(" 1e-1 "), Ok(0.1));
        assert_eq!(parse_signal_value("on"), Ok(1.0));
        assert_eq!(parse_signal_value("Off"), Ok(0.0));
        assert!(parse_signal_value("nan").is_err());
        assert!(parse_signal_value("inf").is_err());
        assert!(parse_signal_value("lots").is_err());
    }

    #[test]
    fn signal_names() {
        assert_eq!(
            signal_request("build-failed", None),
            Ok(Request::Pulse {
                name: "build_failed".into()
            })
        );
        assert_eq!(
            signal_request("pulse.Deploy-Done", None),
            Ok(Request::Pulse {
                name: "deploy_done".into()
            })
        );
        assert_eq!(
            signal_request("CPU.Load", Some("0.25")),
            Ok(Request::Signal {
                name: "cpu.load".into(),
                value: 0.25
            })
        );
        assert!(signal_request("nodot", Some("1")).is_err());
        assert!(signal_request("cpu..load", Some("1")).is_err());
        assert!(signal_request("9lives", None).is_err());
    }

    #[test]
    fn command_args() {
        let cli = Cli::try_parse_from([
            "ukiyo",
            "command",
            "--status",
            "-1",
            "--duration",
            "12.5",
            "--",
            "cargo",
            "build",
            "--release",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Command {
                status,
                duration,
                command,
            } => {
                assert_eq!(status, -1);
                assert_eq!(duration, 12.5);
                assert_eq!(command.join(" "), "cargo build --release");
            }
            _ => panic!(),
        }
    }
}
