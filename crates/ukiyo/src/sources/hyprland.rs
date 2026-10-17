//! Hyprland: its request socket (`.socket.sock`, `j/cursorpos`,
//! `j/monitors`, `j/workspaces`) and event socket (`.socket2.sock`). The
//! parsing here is pure and tested on recorded output; the daemon runs the
//! I/O on helper threads so a slow compositor never stalls a frame.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

/// `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE`, if we run under
/// Hyprland.
pub fn instance_dir() -> Option<PathBuf> {
    let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    if sig.is_empty() {
        return None;
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let dir = runtime.join("hypr").join(&sig);
    if dir.join(".socket.sock").exists() {
        return Some(dir);
    }
    // Hyprland before 0.40 used /tmp/hypr.
    let old = PathBuf::from("/tmp/hypr").join(sig);
    old.join(".socket.sock").exists().then_some(old)
}

/// One request on `.socket.sock` (a new connection each time, as hyprctl
/// does). `cmd` like `j/cursorpos`.
pub fn request(dir: &std::path::Path, cmd: &str) -> std::io::Result<String> {
    let mut s = UnixStream::connect(dir.join(".socket.sock"))?;
    s.set_read_timeout(Some(Duration::from_millis(500)))?;
    s.set_write_timeout(Some(Duration::from_millis(500)))?;
    s.write_all(cmd.as_bytes())?;
    let mut out = String::new();
    s.read_to_string(&mut out)?;
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct CursorPos {
    pub x: f64,
    pub y: f64,
}

pub fn parse_cursorpos(json: &str) -> Option<CursorPos> {
    serde_json::from_str(json).ok()
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct WsRef {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub name: String,
}

/// A monitor from `j/monitors`. `width`/`height` are the mode in pixels;
/// `x`/`y` are in the (logical) layout space.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Monitor {
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default = "one")]
    pub scale: f64,
    #[serde(default)]
    pub transform: i64,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default, rename = "activeWorkspace")]
    pub active_workspace: WsRef,
    #[serde(default, rename = "specialWorkspace")]
    pub special_workspace: WsRef,
}

fn one() -> f64 {
    1.0
}

impl Monitor {
    /// Size in layout coordinates (scale and rotation applied).
    pub fn logical_size(&self) -> (f64, f64) {
        let s = if self.scale > 0.0 { self.scale } else { 1.0 };
        let (w, h) = (self.width / s, self.height / s);
        if self.transform % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        let (w, h) = self.logical_size();
        x >= self.x && y >= self.y && x < self.x + w && y < self.y + h
    }
}

pub fn parse_monitors(json: &str) -> Option<Vec<Monitor>> {
    let v: Vec<Monitor> = serde_json::from_str(json).ok()?;
    Some(v.into_iter().filter(|m| !m.disabled).collect())
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Workspace {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub monitor: String,
    #[serde(default, rename = "hasfullscreen")]
    pub has_fullscreen: bool,
}

pub fn parse_workspaces(json: &str) -> Option<Vec<Workspace>> {
    serde_json::from_str(json).ok()
}

/// The cursor relative to the monitor under it: (name, x, y) with x, y in
/// -1..1 (left/top edge .. right/bottom edge).
pub fn cursor_on(monitors: &[Monitor], p: CursorPos) -> Option<(String, f32, f32)> {
    let m = monitors.iter().find(|m| m.contains(p.x, p.y))?;
    let (w, h) = m.logical_size();
    let nx = ((p.x - m.x) / w.max(1.0)) * 2.0 - 1.0;
    let ny = ((p.y - m.y) / h.max(1.0)) * 2.0 - 1.0;
    Some((
        m.name.clone(),
        nx.clamp(-1.0, 1.0) as f32,
        ny.clamp(-1.0, 1.0) as f32,
    ))
}

/// Names of the monitors whose visible workspace has a fullscreen window
/// (a special workspace shown on top counts too).
pub fn fullscreen_monitors(monitors: &[Monitor], workspaces: &[Workspace]) -> Vec<String> {
    let fs = |id: i64| id != 0 && workspaces.iter().any(|w| w.id == id && w.has_fullscreen);
    monitors
        .iter()
        .filter(|m| fs(m.active_workspace.id) || fs(m.special_workspace.id))
        .map(|m| m.name.clone())
        .collect()
}

/// `workspace.pan` for a workspace id: 1 → -1, 9 and on → 1.
pub fn workspace_pan(id: i64) -> f32 {
    (((id - 1) as f32 / 8.0).clamp(0.0, 1.0)) * 2.0 - 1.0
}

/// What the daemon cares about on the event socket.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The focused monitor shows workspace `id` now.
    Workspace { id: i64 },
    /// Focus moved to `monitor`, which shows workspace `id` (if known).
    FocusedMonitor { monitor: String, id: Option<i64> },
    /// Something that can change which outputs are covered by a fullscreen
    /// window, or the monitor layout: re-query `j/monitors` + `j/workspaces`.
    Refresh,
}

/// Parse one line of `.socket2.sock` (`EVENT>>DATA`).
pub fn parse_event(line: &str) -> Option<Event> {
    let (name, data) = line.trim_end().split_once(">>")?;
    match name {
        "workspacev2" => {
            let id = data.split(',').next()?.trim().parse().ok()?;
            Some(Event::Workspace { id })
        }
        // Older Hyprland: the name only; numeric names are the common case.
        "workspace" => data
            .trim()
            .parse()
            .ok()
            .map(|id| Event::Workspace { id })
            .or(Some(Event::Refresh)),
        "focusedmonv2" => {
            let (mon, id) = data.rsplit_once(',')?;
            Some(Event::FocusedMonitor {
                monitor: mon.to_string(),
                id: id.trim().parse().ok(),
            })
        }
        "focusedmon" => {
            let (mon, _) = data.split_once(',')?;
            Some(Event::FocusedMonitor {
                monitor: mon.to_string(),
                id: None,
            })
        }
        "fullscreen" | "monitoradded" | "monitoraddedv2" | "monitorremoved"
        | "monitorremovedv2" | "activespecial" | "activespecialv2" | "closewindow"
        | "movewindow" | "movewindowv2" | "moveworkspace" | "moveworkspacev2"
        | "destroyworkspace" | "destroyworkspacev2" | "configreloaded" => Some(Event::Refresh),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from Hyprland 0.49 (`hyprctl -j monitors`), trimmed.
    const MONITORS: &str = r#"[{
        "id": 0, "name": "eDP-1", "description": "BOE 0x0BCA", "make": "BOE", "model": "0x0BCA",
        "serial": "", "width": 2880, "height": 1800, "refreshRate": 120.00000,
        "x": 0, "y": 0,
        "activeWorkspace": {"id": 3, "name": "3"},
        "specialWorkspace": {"id": 0, "name": ""},
        "reserved": [0, 32, 0, 0], "scale": 2.00, "transform": 0, "focused": true,
        "dpmsStatus": true, "vrr": false, "solitary": "0", "activelyTearing": false,
        "disabled": false, "currentFormat": "XRGB8888", "mirrorOf": "none",
        "availableModes": ["2880x1800@120.00Hz"]
    },{
        "id": 1, "name": "DP-2", "description": "Dell U2720Q", "width": 3840, "height": 2160,
        "refreshRate": 60.0, "x": 1440, "y": -300,
        "activeWorkspace": {"id": 7, "name": "7"},
        "specialWorkspace": {"id": -98, "name": "special:scratch"},
        "scale": 1.5, "transform": 0, "focused": false, "disabled": false
    },{
        "id": 2, "name": "HDMI-A-1", "width": 1920, "height": 1080, "x": 4000, "y": 0,
        "activeWorkspace": {"id": 9, "name": "9"}, "specialWorkspace": {"id": 0, "name": ""},
        "scale": 1.0, "transform": 1, "focused": false, "disabled": false
    }]"#;

    const WORKSPACES: &str = r#"[
        {"id": 3, "name": "3", "monitor": "eDP-1", "monitorID": 0, "windows": 2,
         "hasfullscreen": true, "lastwindow": "0x55d1", "lastwindowtitle": "mpv"},
        {"id": 7, "name": "7", "monitor": "DP-2", "monitorID": 1, "windows": 1,
         "hasfullscreen": false, "lastwindow": "0x55d2", "lastwindowtitle": "kitty"},
        {"id": -98, "name": "special:scratch", "monitor": "DP-2", "monitorID": 1,
         "windows": 1, "hasfullscreen": false, "lastwindow": "0x0", "lastwindowtitle": ""},
        {"id": 5, "name": "5", "monitor": "eDP-1", "monitorID": 0, "windows": 1,
         "hasfullscreen": true, "lastwindow": "0x0", "lastwindowtitle": ""}
    ]"#;

    #[test]
    fn monitors_and_geometry() {
        let m = parse_monitors(MONITORS).unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].logical_size(), (1440.0, 900.0));
        assert_eq!(m[1].logical_size(), (2560.0, 1440.0));
        assert_eq!(m[2].logical_size(), (1080.0, 1920.0), "rotated");
        assert_eq!(m[1].special_workspace.id, -98);
        assert!(m[0].focused);
    }

    #[test]
    fn cursor_maps_to_the_monitor_under_it() {
        let m = parse_monitors(MONITORS).unwrap();
        let c = parse_cursorpos(r#"{"x": 720, "y": 450}"#).unwrap();
        assert_eq!(cursor_on(&m, c), Some(("eDP-1".into(), 0.0, 0.0)));
        let c = parse_cursorpos("{\n\t\"x\": 0,\n\t\"y\": 899\n}").unwrap();
        let (n, x, y) = cursor_on(&m, c).unwrap();
        assert_eq!(n, "eDP-1");
        assert_eq!(x, -1.0);
        assert!(y > 0.99);
        let c = CursorPos {
            x: 1440.0 + 2560.0 * 0.75,
            y: -300.0,
        };
        assert_eq!(cursor_on(&m, c), Some(("DP-2".into(), 0.5, -1.0)));
        assert_eq!(cursor_on(&m, CursorPos { x: -5.0, y: 0.0 }), None);
        assert!(parse_cursorpos("unknown request").is_none());
    }

    #[test]
    fn fullscreen_per_monitor() {
        let m = parse_monitors(MONITORS).unwrap();
        let w = parse_workspaces(WORKSPACES).unwrap();
        assert_eq!(fullscreen_monitors(&m, &w), ["eDP-1"]);
        let mut w2 = w.clone();
        w2[0].has_fullscreen = false;
        w2[2].has_fullscreen = true;
        assert_eq!(
            fullscreen_monitors(&m, &w2),
            ["DP-2"],
            "special workspace on top"
        );
    }

    #[test]
    fn events() {
        // Recorded from `socat - UNIX-CONNECT:.socket2.sock`.
        let lines = "workspace>>4\nworkspacev2>>4,4\nfocusedmon>>DP-2,7\nfocusedmonv2>>DP-2,7\n\
                     activewindow>>kitty,~\nactivewindowv2>>55d2\nfullscreen>>1\n\
                     monitoraddedv2>>2,HDMI-A-1,Some Monitor\nmonitorremoved>>HDMI-A-1\n\
                     openwindow>>55d3,4,kitty,~\nclosewindow>>55d3\nworkspacev2>>12,web\n\
                     workspace>>name:music\nbogus\n";
        let ev: Vec<_> = lines.lines().map(parse_event).collect();
        assert_eq!(
            ev,
            [
                Some(Event::Workspace { id: 4 }),
                Some(Event::Workspace { id: 4 }),
                Some(Event::FocusedMonitor {
                    monitor: "DP-2".into(),
                    id: None
                }),
                Some(Event::FocusedMonitor {
                    monitor: "DP-2".into(),
                    id: Some(7)
                }),
                None,
                None,
                Some(Event::Refresh),
                Some(Event::Refresh),
                Some(Event::Refresh),
                None,
                Some(Event::Refresh),
                Some(Event::Workspace { id: 12 }),
                Some(Event::Refresh),
                None,
            ]
        );
    }

    #[test]
    fn pan() {
        assert_eq!(workspace_pan(1), -1.0);
        assert_eq!(workspace_pan(5), 0.0);
        assert_eq!(workspace_pan(9), 1.0);
        assert_eq!(workspace_pan(42), 1.0);
        assert_eq!(workspace_pan(-98), -1.0, "special workspaces");
    }
}
