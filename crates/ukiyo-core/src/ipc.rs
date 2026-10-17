//! The control protocol: one JSON object per line over a Unix socket
//! (`$XDG_RUNTIME_DIR/ukiyo/<WAYLAND_DISPLAY>.sock`). A request gets exactly
//! one response line. Small enough to speak from a shell:
//!
//! ```sh
//! echo '{"cmd":"pulse","name":"build_failed"}' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/ukiyo/wayland-1.sock
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Longest request line the daemon reads (bytes).
pub const MAX_LINE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    /// Show a scene (an id from `scenes`, or a directory).
    Set {
        scene: String,
    },
    /// Daemon state, outputs, signals and bindings.
    Status,
    /// Installed scenes.
    Scenes,
    /// Write a signal's raw value.
    Signal {
        name: String,
        value: f32,
    },
    /// Fire `pulse.<name>`.
    Pulse {
        name: String,
    },
    /// A shell command finished. The daemon decides (per
    /// `signals.build.*`) whether it was a build and pulses
    /// `build_passed` or `build_failed`.
    Command {
        command: String,
        status: i32,
        duration: f32,
    },
    /// Stop drawing now; the last frame stays.
    Pause {
        #[serde(default)]
        reason: Option<String>,
    },
    /// Undo `pause` (and `still`).
    Resume {
        #[serde(default)]
        reason: Option<String>,
    },
    /// Ease to a standstill, then stop drawing (lock screen).
    Still,
    /// Save the current frame of every output as PNG; returns the paths.
    Snapshot {
        #[serde(default)]
        dir: Option<PathBuf>,
    },
    ConfigGet {
        key: String,
    },
    /// Set and persist to the user config. `value` is JSON (string, number,
    /// bool, list); strings are parsed per the option's type.
    ConfigSet {
        key: String,
        value: serde_json::Value,
    },
    /// Remove from the user config: an option, or `scenes.<id>` for all of
    /// a scene's overrides.
    ConfigReset {
        key: String,
    },
    /// Every option with type, range, default, current value and label.
    ConfigSchema {
        #[serde(default)]
        scene: Option<String>,
    },
    /// Re-read the config files now.
    Reload,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub data: serde_json::Value,
}

impl Response {
    pub fn ok(data: impl Serialize) -> Response {
        Response {
            ok: true,
            error: None,
            data: serde_json::to_value(data).unwrap_or(serde_json::Value::Null),
        }
    }

    pub fn empty() -> Response {
        Response {
            ok: true,
            error: None,
            data: serde_json::Value::Null,
        }
    }

    pub fn err(msg: impl Into<String>) -> Response {
        Response {
            ok: false,
            error: Some(msg.into()),
            data: serde_json::Value::Null,
        }
    }
}

/// One request as a line (with the trailing newline).
pub fn encode<T: Serialize>(msg: &T) -> String {
    let mut s = serde_json::to_string(msg).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

pub fn decode_request(line: &str) -> Result<Request, String> {
    if line.len() > MAX_LINE {
        return Err(format!("request longer than {MAX_LINE} bytes"));
    }
    serde_json::from_str(line.trim()).map_err(|e| format!("bad request: {e}"))
}

pub fn decode_response(line: &str) -> Result<Response, String> {
    serde_json::from_str(line.trim()).map_err(|e| format!("bad response: {e}"))
}

/// The socket path for a Wayland display: `$XDG_RUNTIME_DIR/ukiyo/<display>.sock`.
pub fn socket_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("UKIYO_SOCKET") {
        return Some(PathBuf::from(p));
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    let display = display
        .rsplit('/')
        .next()
        .unwrap_or("wayland-0")
        .to_string();
    Some(
        PathBuf::from(runtime)
            .join("ukiyo")
            .join(format!("{display}.sock")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let all = vec![
            Request::Set {
                scene: "kabukicho-gate".into(),
            },
            Request::Status,
            Request::Scenes,
            Request::Signal {
                name: "cpu.load".into(),
                value: 0.5,
            },
            Request::Pulse {
                name: "build_failed".into(),
            },
            Request::Command {
                command: "cargo build".into(),
                status: 101,
                duration: 12.5,
            },
            Request::Pause {
                reason: Some("fullscreen".into()),
            },
            Request::Resume { reason: None },
            Request::Still,
            Request::Snapshot {
                dir: Some("/tmp/x".into()),
            },
            Request::ConfigGet {
                key: "intensity".into(),
            },
            Request::ConfigSet {
                key: "intensity".into(),
                value: serde_json::json!(0.5),
            },
            Request::ConfigReset {
                key: "scenes.kabukicho-gate".into(),
            },
            Request::ConfigSchema { scene: None },
            Request::Reload,
            Request::Quit,
        ];
        for r in all {
            let line = encode(&r);
            assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
            assert_eq!(decode_request(&line).unwrap(), r);
        }
    }

    #[test]
    fn wire_format_is_stable() {
        assert_eq!(encode(&Request::Status), "{\"cmd\":\"status\"}\n");
        assert_eq!(
            encode(&Request::Pulse {
                name: "build_failed".into()
            }),
            "{\"cmd\":\"pulse\",\"name\":\"build_failed\"}\n"
        );
        assert_eq!(
            decode_request(r#"{"cmd":"config-set","key":"intensity","value":"0.5"}"#).unwrap(),
            Request::ConfigSet {
                key: "intensity".into(),
                value: serde_json::json!("0.5")
            }
        );
        // Optional fields may be left out.
        assert_eq!(
            decode_request(r#"{"cmd":"pause"}"#).unwrap(),
            Request::Pause { reason: None }
        );
    }

    #[test]
    fn bad_requests() {
        assert!(
            decode_request("{\"cmd\":\"explode\"}")
                .unwrap_err()
                .contains("bad request")
        );
        assert!(decode_request("not json").is_err());
        assert!(
            decode_request(r#"{"cmd":"signal","name":"x"}"#).is_err(),
            "missing value"
        );
        assert!(
            decode_request(&"x".repeat(MAX_LINE + 1))
                .unwrap_err()
                .contains("longer")
        );
    }

    #[test]
    fn responses() {
        let ok = Response::ok(serde_json::json!({"scene": "x"}));
        assert_eq!(decode_response(&encode(&ok)).unwrap(), ok);
        assert_eq!(encode(&Response::empty()), "{\"ok\":true}\n");
        let err = Response::err("nope");
        assert_eq!(encode(&err), "{\"ok\":false,\"error\":\"nope\"}\n");
    }
}
