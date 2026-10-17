//! The daemon: one calloop event loop driving the Wayland connection, the
//! IPC socket, the signal sources and the frame pacing.
//!
//! Frame pacing: a frame round is due when some output is ready (its last
//! frame callback has arrived) and `frame_interval()` has passed since the
//! last round; a one-shot timer waits out the remainder. A round ticks the
//! engine once and draws every ready output, requesting a frame callback
//! before each present. When nothing wants a frame (paused, fully still, a
//! static scene at rest) no timer is armed and no callback requested: the
//! daemon sleeps until an event (IPC, a compositor event, a source) wakes
//! it. Outputs the compositor hides never call back, so they cost nothing.

mod gfx;
mod ipcsrv;
mod wl;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use serde_json::{Value as Json, json};
use smithay_client_toolkit::reexports::calloop::{
    self, EventLoop, LoopHandle, RegistrationToken,
    channel::{self, Channel},
    timer::{TimeoutAction, Timer},
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_pointer, wl_seat},
};
use smithay_client_toolkit::{
    compositor::CompositorState,
    output::OutputState,
    registry::RegistryState,
    seat::SeatState,
    shell::{WaylandSurface, wlr_layer::LayerShell},
};
use ukiyo_core::config::{self, Config, Paths};
use ukiyo_core::ipc::{self, Request, Response};
use ukiyo_core::scene::Scene;
use ukiyo_core::{Engine, Frame, catalog, signals};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::ExtIdleNotificationV1, ext_idle_notifier_v1::ExtIdleNotifierV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::confops;
use crate::sources::{audio, cpu, hyprland, power, typing};
use gfx::{Drawn, Gfx};
use wl::{IdleKind, NoEvents, Output};

/// Housekeeping cadence: config files, battery.
const TICK: Duration = Duration::from_secs(2);
/// Battery is polled every this many ticks.
const BATTERY_TICKS: u32 = 5;
/// Probe for the typing helper (source `auto`) every this many ticks.
const HELPER_PROBE_TICKS: u32 = 15;
/// How often sources are read while a static scene waits for a change.
const STATIC_POLL: Duration = Duration::from_millis(250);

/// What helper threads send to the loop.
pub enum Msg {
    Event(hyprland::Event),
    Cursor(Option<hyprland::CursorPos>),
    Layout(
        Option<Vec<hyprland::Monitor>>,
        Option<Vec<hyprland::Workspace>>,
    ),
}

enum HyprReq {
    Cursor,
    Refresh,
}

struct Hypr {
    tx: mpsc::Sender<HyprReq>,
    cursor_in_flight: bool,
    refresh_in_flight: bool,
    refresh_again: bool,
    monitors: Vec<hyprland::Monitor>,
    fullscreen: Vec<String>,
}

impl Hypr {
    fn start(dir: PathBuf, msgs: channel::Sender<Msg>) -> Hypr {
        let (tx, rx) = mpsc::channel::<HyprReq>();
        let qdir = dir.clone();
        let qmsgs = msgs.clone();
        let _ = std::thread::Builder::new()
            .name("ukiyo-hypr-query".into())
            .spawn(move || {
                for req in rx {
                    let msg = match req {
                        HyprReq::Cursor => Msg::Cursor(
                            hyprland::request(&qdir, "j/cursorpos")
                                .ok()
                                .and_then(|s| hyprland::parse_cursorpos(&s)),
                        ),
                        HyprReq::Refresh => Msg::Layout(
                            hyprland::request(&qdir, "j/monitors")
                                .ok()
                                .and_then(|s| hyprland::parse_monitors(&s)),
                            hyprland::request(&qdir, "j/workspaces")
                                .ok()
                                .and_then(|s| hyprland::parse_workspaces(&s)),
                        ),
                    };
                    if qmsgs.send(msg).is_err() {
                        break;
                    }
                }
            });
        let _ = std::thread::Builder::new()
            .name("ukiyo-hypr-events".into())
            .spawn(move || {
                use std::io::BufRead;
                let Ok(s) = std::os::unix::net::UnixStream::connect(dir.join(".socket2.sock"))
                else {
                    log::warn!("hyprland: can't open the event socket");
                    return;
                };
                for line in std::io::BufReader::new(s).lines() {
                    let Ok(line) = line else { break };
                    if let Some(ev) = hyprland::parse_event(&line)
                        && msgs.send(Msg::Event(ev)).is_err()
                    {
                        break;
                    }
                }
                log::info!("hyprland: event socket closed");
            });
        let h = Hypr {
            tx,
            cursor_in_flight: false,
            refresh_in_flight: false,
            refresh_again: false,
            monitors: Vec::new(),
            fullscreen: Vec::new(),
        };
        h.tx.send(HyprReq::Refresh).ok();
        Hypr {
            refresh_in_flight: true,
            ..h
        }
    }

    fn refresh(&mut self) {
        if self.refresh_in_flight {
            self.refresh_again = true;
        } else if self.tx.send(HyprReq::Refresh).is_ok() {
            self.refresh_in_flight = true;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorMode {
    Off,
    Hyprland,
    Pointer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypingMode {
    Off,
    Idle,
    Helper,
}

pub struct State {
    // Wayland.
    conn: Connection,
    qh: QueueHandle<State>,
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    viewporter: Option<WpViewporter>,
    fractional_mgr: Option<WpFractionalScaleManagerV1>,
    idle_notifier: Option<ExtIdleNotifierV1>,
    seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    idle_pause: Option<(ExtIdleNotificationV1, u32)>,
    idle_typing: Option<ExtIdleNotificationV1>,
    /// Before `gfx`: every output's GPU surface goes before the device.
    outputs: Vec<Output>,
    gfx: Gfx,

    // Engine and config.
    engine: Engine,
    paths: Paths,
    config: Config,
    stamps: Vec<Option<(SystemTime, u64)>>,
    warned: HashSet<String>,
    /// Signal groups the scene's bindings read.
    uses: HashSet<String>,
    reasons: BTreeSet<String>,
    want_still: bool,
    on_battery: bool,
    workspace_id: Option<i64>,

    // Pacing.
    handle: LoopHandle<'static, State>,
    epoch: Instant,
    timer: Option<RegistrationToken>,
    last_round: Option<f64>,
    last_frame: Option<Frame>,
    /// A lock frame was asked for but nothing was drawn yet.
    still_pending: bool,
    still_frames: Vec<PathBuf>,
    /// Bumped on every lock-frame write, so a lock screen knows it's fresh.
    still_serial: u64,

    // Sources.
    hypr: Option<Hypr>,
    cursor_mode: CursorMode,
    last_cursor: Option<(f32, f32)>,
    typing_mode: TypingMode,
    idle_typing_est: typing::IdleTyping,
    helper: Option<typing::HelperClient>,
    helper_rate: Arc<Mutex<Option<f32>>>,
    cpu: cpu::Sampler,
    last_cpu: f64,
    audio: Option<audio::Capture>,
    audio_name: String,
    audio_failed: bool,
    audio_warned: bool,
    audio_levels: Arc<Mutex<audio::Levels>>,
    ticks: u32,
    sources_line: String,

    exit: bool,
    fatal: Option<anyhow::Error>,
}

/// Run the daemon until `quit`, SIGTERM or SIGINT.
pub fn run(scene_arg: Option<String>) -> Result<()> {
    // Block the signals before any thread exists, so they all reach us.
    let signals = calloop::signals::Signals::new(&[
        calloop::signals::Signal::SIGTERM,
        calloop::signals::Signal::SIGINT,
    ])
    .context("signal handling")?;

    let paths = Paths::from_env();
    let (config, warnings) = Config::load(&paths);
    for w in &warnings {
        log::warn!("{w}");
    }
    let wanted = scene_arg.or_else(|| config_scene(&config));
    let dirs = catalog::search_path();
    let dir = match &wanted {
        Some(w) => catalog::find(&dirs, w)
            .with_context(|| format!("no scene `{w}` (see `ukiyo scenes`)"))?,
        None => catalog::list(&dirs)
            .into_iter()
            .next()
            .map(|e| e.dir)
            .context("no scenes installed (put one in ~/.local/share/ukiyo/scenes)")?,
    };
    let scene = Scene::load(&dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    let engine = new_engine(scene, &config);
    log::info!(
        "scene {} ({})",
        engine.scene.manifest.scene.name,
        engine.scene.id
    );

    let sock = ipc::socket_path().context("XDG_RUNTIME_DIR isn't set")?;
    let listener = ipcsrv::bind(&sock)?;

    let conn = Connection::connect_to_env().context("can't connect to the Wayland compositor")?;
    let (globals, queue) = registry_queue_init(&conn).context("Wayland registry")?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue)
        .insert(handle.clone())
        .map_err(|e| anyhow::anyhow!("wayland source: {e}"))?;

    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor is missing")?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .context("the compositor has no wlr-layer-shell (needed to draw wallpapers)")?;
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, NoEvents).ok();
    let fractional_mgr: Option<WpFractionalScaleManagerV1> =
        globals.bind(&qh, 1..=1, NoEvents).ok();
    let idle_notifier: Option<ExtIdleNotifierV1> = globals.bind(&qh, 1..=2, NoEvents).ok();
    if idle_notifier.is_none() {
        log::info!("no ext-idle-notify: idle pause and the idle typing source are off");
    }

    let (msgs, msg_rx): (channel::Sender<Msg>, Channel<Msg>) = channel::channel();
    handle
        .insert_source(msg_rx, |e, _, state: &mut State| {
            if let channel::Event::Msg(m) = e {
                state.on_msg(m);
            }
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    handle
        .insert_source(signals, |e, _, state: &mut State| {
            log::info!("{:?}: quitting", e.signal());
            state.exit = true;
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    handle
        .insert_source(Timer::from_duration(TICK), |_, _, state: &mut State| {
            state.housekeeping();
            TimeoutAction::ToDuration(TICK)
        })
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    ipcsrv::insert(&handle, listener)?;

    let display = conn.backend().display_ptr() as *mut std::ffi::c_void;
    let hypr = hyprland::instance_dir().map(|d| {
        log::info!("hyprland: {}", d.display());
        Hypr::start(d, msgs)
    });
    let mut state = State {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        conn,
        qh,
        compositor,
        layer_shell,
        viewporter,
        fractional_mgr,
        idle_notifier,
        seat: None,
        pointer: None,
        idle_pause: None,
        idle_typing: None,
        outputs: Vec::new(),
        gfx: Gfx::new(display)?,
        uses: HashSet::new(),
        engine,
        stamps: stamps(&paths),
        paths,
        config,
        warned: warnings.into_iter().collect(),
        reasons: BTreeSet::new(),
        want_still: false,
        on_battery: false,
        workspace_id: None,
        handle,
        epoch: Instant::now(),
        timer: None,
        last_round: None,
        last_frame: None,
        still_pending: false,
        still_frames: Vec::new(),
        still_serial: 0,
        hypr,
        cursor_mode: CursorMode::Off,
        last_cursor: None,
        typing_mode: TypingMode::Off,
        idle_typing_est: typing::IdleTyping::new(),
        helper: None,
        helper_rate: Arc::new(Mutex::new(None)),
        cpu: cpu::Sampler::default(),
        last_cpu: f64::NEG_INFINITY,
        audio: None,
        audio_name: String::new(),
        audio_failed: false,
        audio_warned: false,
        audio_levels: Arc::new(Mutex::new(audio::Levels::default())),
        ticks: 0,
        sources_line: String::new(),
        exit: false,
        fatal: None,
    };
    // Seats present at startup don't go through `new_seat`.
    state.seat = state.seat_state.seats().next();
    state.warn_config();
    state.on_battery = power::on_battery_now();
    state.engine.set_on_battery(state.on_battery);
    state.scene_changed();
    log::info!("listening on {}", sock.display());

    let result = loop {
        if let Err(e) = event_loop.dispatch(None, &mut state) {
            break Err(anyhow::anyhow!("event loop: {e}"));
        }
        if let Some(e) = state.fatal.take() {
            break Err(e);
        }
        if state.exit {
            break Ok(());
        }
    };
    // GPU surfaces before their Wayland surfaces, all before the connection.
    state.audio = None;
    state.helper = None;
    state.outputs.clear();
    let _ = state.conn.flush();
    let _ = std::fs::remove_file(&sock);
    result
}

fn config_scene(cfg: &Config) -> Option<String> {
    ukiyo_core::settings::Settings::from_config(cfg, None).scene
}

fn new_engine(scene: Scene, config: &Config) -> Engine {
    let e = Engine::new(scene, config);
    for u in &e.unknown_names {
        log::warn!("scene {}: binding {u} reads an unknown name", e.scene.id);
    }
    e
}

/// mtime + length of every config file (None: missing).
fn stamps(paths: &Paths) -> Vec<Option<(SystemTime, u64)>> {
    paths
        .system
        .iter()
        .chain([&paths.defaults, &paths.user])
        .map(|p| {
            std::fs::metadata(p)
                .ok()
                .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
        })
        .collect()
}

fn runtime_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("ukiyo"))
}

/// Write a PNG atomically (tmp + rename), with fast compression.
fn write_png(path: &Path, img: &image::RgbaImage) -> Result<()> {
    use image::ImageEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    let tmp = path.with_extension("png.tmp");
    {
        let f = std::fs::File::create(&tmp).with_context(|| tmp.display().to_string())?;
        let w = std::io::BufWriter::new(f);
        PngEncoder::new_with_quality(w, CompressionType::Fast, FilterType::Sub)
            .write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                image::ExtendedColorType::Rgba8,
            )
            .with_context(|| tmp.display().to_string())?;
    }
    std::fs::rename(&tmp, path).with_context(|| path.display().to_string())?;
    Ok(())
}

/// A file name part from an output name.
fn safe_name(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl State {
    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64()
    }

    /// Drawing continuously: nothing pauses it and it isn't going still.
    fn active(&self) -> bool {
        self.reasons.is_empty() && !self.engine.target_still()
    }

    fn uses(&self, group: &str) -> bool {
        self.uses.contains(group)
    }

    // ---- pacing ---------------------------------------------------------

    /// Arm the frame timer if a round is due (see the module docs).
    pub fn schedule(&mut self) {
        if self.timer.is_some() || self.exit {
            return;
        }
        let animate = self.reasons.is_empty() && self.engine.wants_frame();
        let due = self
            .outputs
            .iter()
            .any(|o| o.ready() && (o.needs_draw || (animate && !o.covered)));
        let now = self.now();
        let at = if due {
            let interval = if animate {
                self.engine.frame_interval()
            } else {
                0.0
            };
            self.last_round.map_or(now, |l| (l + interval).max(now))
        } else if self.active()
            && !self.engine.scene.manifest.render.animated
            && self.outputs.iter().any(|o| o.configured)
        {
            // A static scene at rest: look at the sources now and then.
            now + STATIC_POLL.as_secs_f64()
        } else {
            return;
        };
        let deadline = self.epoch + Duration::from_secs_f64(at);
        let token =
            self.handle
                .insert_source(Timer::from_deadline(deadline), |_, _, state: &mut State| {
                    state.timer = None;
                    state.round();
                    state.schedule();
                    TimeoutAction::Drop
                });
        self.timer = token.ok();
    }

    /// One frame: read the sources, tick the engine, draw ready outputs.
    fn round(&mut self) {
        let now = self.now();
        if self.reasons.is_empty() {
            self.feed_signals(now);
        }
        let animate = self.reasons.is_empty() && self.engine.wants_frame();
        let targets: Vec<usize> = (0..self.outputs.len())
            .filter(|&i| {
                let o = &self.outputs[i];
                o.ready() && (o.needs_draw || (animate && !o.covered))
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        let frame = self.engine.tick(now);
        let mut redo = Vec::new();
        for i in targets {
            let qh = self.qh.clone();
            let Some(dev) = &self.gfx.dev else { break };
            let o = &mut self.outputs[i];
            let Some(g) = &mut o.gpu else { continue };
            let surface = o.layer.wl_surface().clone();
            match gfx::draw(g, dev, &frame, || {
                surface.frame(
                    &qh,
                    smithay_client_toolkit::compositor::FrameCallbackData(surface.clone()),
                );
            }) {
                Drawn::Yes => {
                    o.frame_pending = true;
                    o.needs_draw = false;
                    o.fps.tick(now);
                }
                Drawn::Skipped => {}
                Drawn::Reconfigure => redo.push(i),
            }
        }
        for i in redo {
            self.reconfigure(i);
        }
        self.last_round = Some(now);
        self.last_frame = Some(frame);
        self.check_still();
    }

    /// Continuous sources into the bus, once per frame.
    fn feed_signals(&mut self, now: f64) {
        let bus = &mut self.engine.bus;
        if self.audio.is_some() {
            let l = self.audio_levels.lock().map(|l| *l).unwrap_or_default();
            bus.set("audio.level", l.level);
            bus.set("audio.bass", l.bass);
            bus.set("audio.mid", l.mid);
            bus.set("audio.treble", l.treble);
            bus.set("audio.beat", l.beat);
        }
        match self.typing_mode {
            TypingMode::Helper if self.helper.is_some() => {
                let r = self.helper_rate.lock().ok().and_then(|r| *r).unwrap_or(0.0);
                bus.set("typing.rate", typing::helper_rate(r));
            }
            TypingMode::Idle => {
                let r = self.idle_typing_est.rate(now);
                bus.set("typing.rate", r);
            }
            _ => {}
        }
        if self.uses("cpu") && self.signal_on("cpu") {
            let window = f64::from(self.engine.settings().cpu_window.max(0.25));
            if now - self.last_cpu >= window / 4.0 {
                self.last_cpu = now;
                if let Some(t) = cpu::Sampler::read()
                    && let Some(load) = self.cpu.push(now, t, window)
                {
                    self.engine.bus.set("cpu.load", load);
                }
            }
        }
        if self.cursor_mode == CursorMode::Hyprland
            && let Some(h) = &mut self.hypr
            && !h.cursor_in_flight
            && h.tx.send(HyprReq::Cursor).is_ok()
        {
            h.cursor_in_flight = true;
        }
    }

    fn signal_on(&self, group: &str) -> bool {
        self.engine
            .settings()
            .signal(group)
            .is_none_or(|s| s.enabled)
    }

    // ---- outputs --------------------------------------------------------

    /// (Re)configure output `i`'s swapchain and renderer for its size and
    /// scale, creating the GPU side on first use.
    fn reconfigure(&mut self, i: usize) {
        let o = &mut self.outputs[i];
        if !o.configured {
            return;
        }
        if o.gpu.is_none() {
            let ptr = o.layer.wl_surface().id().as_ptr() as *mut std::ffi::c_void;
            // SAFETY: the wl_surface lives in `o.layer`, and `Output` drops
            // its GPU side first.
            match unsafe { self.gfx.create_surface(ptr) } {
                Ok(s) => o.gpu = Some(gfx::OutGpu::new(s)),
                Err(e) => {
                    log::error!("output {}: {e:#}", o.name);
                    return;
                }
            }
        }
        o.apply_scale();
        let size = o.buffer_size();
        let Some(g) = &mut o.gpu else { return };
        if let Err(e) = self.gfx.ensure_device(&g.surface, &self.engine) {
            self.fatal = Some(e.context(format!("scene {}", self.engine.scene.id)));
            return;
        }
        let (Some(dev), Some(scene)) = (&self.gfx.dev, &self.gfx.scene) else {
            return;
        };
        let scale = self.engine.fps_and_scale().1;
        match g.configure(dev, scene, size, scale) {
            Ok(()) => {
                log::debug!(
                    "output {}: {}x{} logical, scale {}, buffer {}x{}",
                    o.name,
                    o.logical.0,
                    o.logical.1,
                    o.scale(),
                    size.0,
                    size.1
                );
                o.needs_draw = true;
            }
            Err(e) => log::error!("output {}: {e:#}", o.name),
        }
    }

    /// Render scale changed (battery, config): resize every renderer.
    fn rescale_all(&mut self) {
        for i in 0..self.outputs.len() {
            if self.outputs[i].gpu.is_some() {
                self.reconfigure(i);
            }
        }
    }

    /// Which outputs a fullscreen window covers, and the `fullscreen`
    /// reason when that's all of them.
    fn update_covered(&mut self) {
        let on = self.engine.settings().pause_on_fullscreen;
        let fs = self
            .hypr
            .as_ref()
            .map(|h| h.fullscreen.clone())
            .unwrap_or_default();
        for o in &mut self.outputs {
            let c = on && fs.contains(&o.name);
            if c != o.covered {
                o.covered = c;
                if !c {
                    o.needs_draw = true;
                }
            }
        }
        let all = !self.outputs.is_empty() && self.outputs.iter().all(|o| o.covered);
        self.set_reason("fullscreen", all);
    }

    fn set_reason(&mut self, reason: &str, on: bool) {
        let changed = if on {
            self.reasons.insert(reason.to_string())
        } else {
            self.reasons.remove(reason)
        };
        if changed {
            log::info!("{} {reason}", if on { "paused:" } else { "resumed:" });
            self.update_sources();
        }
    }

    // ---- idle -----------------------------------------------------------

    /// Create or drop the idle notifications to match the settings.
    pub fn update_idle(&mut self, qh: &QueueHandle<State>) {
        let timeout_ms = (self.engine.settings().idle_timeout * 1000.0) as u32;
        let want_pause = (timeout_ms > 0 && self.seat.is_some() && self.idle_notifier.is_some())
            .then_some(timeout_ms);
        if self.idle_pause.as_ref().map(|(_, t)| *t) != want_pause {
            if let Some((n, _)) = self.idle_pause.take() {
                n.destroy();
            }
            self.set_reason("idle", false);
            if let (Some(t), Some(notifier), Some(seat)) =
                (want_pause, &self.idle_notifier, &self.seat)
            {
                // Honour inhibitors (a video playing): not idle then.
                let n = notifier.get_idle_notification(t, seat, qh, IdleKind::Pause);
                self.idle_pause = Some((n, t));
            }
        }
        let want_typing = self.typing_mode == TypingMode::Idle && self.active();
        if want_typing != self.idle_typing.is_some() {
            if let Some(n) = self.idle_typing.take() {
                n.destroy();
            }
            if want_typing && let (Some(notifier), Some(seat)) = (&self.idle_notifier, &self.seat) {
                let n = if notifier.version() >= 2 {
                    // Input only: inhibitors don't make us think you type.
                    notifier.get_input_idle_notification(
                        typing::IDLE_TIMEOUT_MS,
                        seat,
                        qh,
                        IdleKind::Typing,
                    )
                } else {
                    notifier.get_idle_notification(
                        typing::IDLE_TIMEOUT_MS,
                        seat,
                        qh,
                        IdleKind::Typing,
                    )
                };
                self.idle_typing = Some(n);
            }
            self.idle_typing_est = typing::IdleTyping::new();
        }
    }

    pub fn on_idle(&mut self, kind: IdleKind, idle: bool) {
        let now = self.now();
        match kind {
            IdleKind::Pause => self.set_reason("idle", idle),
            IdleKind::Typing => {
                if idle {
                    self.idle_typing_est.idled(now);
                } else {
                    self.idle_typing_est.resumed(now);
                }
                self.engine.poke();
            }
        }
        self.schedule();
    }

    /// Cursor over our surface (the `pointer` source).
    pub fn on_pointer(&mut self, x: f32, y: f32) {
        if self.cursor_mode != CursorMode::Pointer {
            return;
        }
        self.cursor_moved(x, y);
        self.schedule();
    }

    fn cursor_moved(&mut self, x: f32, y: f32) {
        if self
            .last_cursor
            .is_none_or(|(lx, ly)| (lx - x).abs() > 1e-3 || (ly - y).abs() > 1e-3)
        {
            let now = self.now();
            self.idle_typing_est.pointer_moved(now);
            self.last_cursor = Some((x, y));
            self.engine.bus.set("cursor.x", x);
            self.engine.bus.set("cursor.y", y);
        }
    }

    // ---- sources --------------------------------------------------------

    fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Cursor(p) => {
                if let Some(h) = &mut self.hypr {
                    h.cursor_in_flight = false;
                }
                if self.cursor_mode == CursorMode::Hyprland
                    && let Some(p) = p
                    && let Some(h) = &self.hypr
                    && let Some((_, x, y)) = hyprland::cursor_on(&h.monitors, p)
                {
                    self.cursor_moved(x, y);
                }
            }
            Msg::Layout(monitors, workspaces) => {
                let Some(h) = &mut self.hypr else { return };
                h.refresh_in_flight = false;
                if let Some(m) = monitors {
                    if let Some(w) = &workspaces {
                        h.fullscreen = hyprland::fullscreen_monitors(&m, w);
                    }
                    let focused = m.iter().find(|m| m.focused).map(|m| m.active_workspace.id);
                    h.monitors = m;
                    if let Some(id) = focused {
                        self.set_workspace(id);
                    }
                }
                let Some(h) = &mut self.hypr else { return };
                if std::mem::take(&mut h.refresh_again) {
                    h.refresh();
                }
                self.update_covered();
            }
            Msg::Event(ev) => {
                match ev {
                    hyprland::Event::Workspace { id }
                    | hyprland::Event::FocusedMonitor { id: Some(id), .. } => {
                        self.set_workspace(id)
                    }
                    _ => {}
                }
                if let Some(h) = &mut self.hypr {
                    h.refresh();
                }
            }
        }
        self.schedule();
    }

    fn set_workspace(&mut self, id: i64) {
        if self.workspace_id == Some(id) {
            return;
        }
        self.workspace_id = Some(id);
        if self.signal_on("workspace") {
            self.engine.bus.set("workspace.id", id as f32);
            self.engine
                .bus
                .set("workspace.pan", hyprland::workspace_pan(id));
            self.engine.poke();
        }
    }

    /// Start and stop sources to match the settings, the scene's needs and
    /// whether we're drawing. Cheap and idempotent.
    fn update_sources(&mut self) {
        let s = self.engine.settings().clone();
        let active = self.active();
        let have_hypr = self.hypr.is_some();

        self.cursor_mode = if !self.uses("cursor") || !self.signal_on("cursor") {
            CursorMode::Off
        } else {
            match s.cursor_source.as_str() {
                "pointer" => CursorMode::Pointer,
                _ if have_hypr => CursorMode::Hyprland,
                _ => CursorMode::Pointer,
            }
        };

        let want_typing = self.uses("typing") && self.signal_on("typing");
        let helper_ok = || typing::helper_available(&s.helper_socket);
        self.typing_mode = match (want_typing, s.typing_source.as_str()) {
            (false, _) => TypingMode::Off,
            (true, "helper") => TypingMode::Helper,
            (true, "idle") => TypingMode::Idle,
            // auto: keep the helper while it works; probe it otherwise.
            (true, _) => {
                if self.typing_mode == TypingMode::Helper
                    && self.helper.as_ref().is_some_and(|h| !h.finished())
                    || helper_ok()
                {
                    TypingMode::Helper
                } else {
                    TypingMode::Idle
                }
            }
        };
        if self.typing_mode == TypingMode::Idle && self.idle_notifier.is_none() {
            self.typing_mode = TypingMode::Off;
        }
        if self.helper.as_ref().is_some_and(|h| h.finished()) {
            self.helper = None;
        }
        if self.typing_mode == TypingMode::Helper && active {
            if self.helper.is_none() {
                match typing::HelperClient::connect(&s.helper_socket, self.helper_rate.clone()) {
                    Ok(c) => self.helper = Some(c),
                    Err(e) => {
                        log::debug!("typing helper {}: {e}", s.helper_socket);
                        if s.typing_source != "helper" && self.idle_notifier.is_some() {
                            self.typing_mode = TypingMode::Idle;
                        }
                    }
                }
            }
        } else {
            self.helper = None;
        }

        let want_audio =
            active && self.uses("audio") && self.signal_on("audio") && !self.audio_failed;
        if want_audio && self.audio.is_none() {
            match audio::Capture::start(&s.audio_device, self.audio_levels.clone()) {
                Some((c, name)) => {
                    self.audio = Some(c);
                    self.audio_name = name;
                }
                None => {
                    if !self.audio_warned {
                        log::info!("audio: neither pw-record nor parec could capture; audio off");
                        self.audio_warned = true;
                    }
                    self.audio_failed = true;
                }
            }
        } else if !want_audio && self.audio.is_some() {
            self.audio = None;
            for k in [
                "audio.level",
                "audio.bass",
                "audio.mid",
                "audio.treble",
                "audio.beat",
            ] {
                self.engine.bus.set(k, 0.0);
            }
        }

        let qh = self.qh.clone();
        self.update_idle(&qh);
        self.log_sources();
    }

    fn sources_json(&self) -> Json {
        let typing = match self.typing_mode {
            TypingMode::Off => "off",
            TypingMode::Idle => "idle",
            TypingMode::Helper => "helper",
        };
        let cursor = match self.cursor_mode {
            CursorMode::Off => "off",
            CursorMode::Hyprland => "hyprland",
            CursorMode::Pointer => "pointer",
        };
        let workspace = if self.hypr.is_some() && self.signal_on("workspace") {
            "hyprland"
        } else {
            "off"
        };
        let cpu = if self.uses("cpu") && self.signal_on("cpu") {
            "proc"
        } else {
            "off"
        };
        let audio = if self.audio.is_some() {
            self.audio_name.as_str()
        } else {
            "off"
        };
        json!({
            "cursor": cursor,
            "workspace": workspace,
            "typing": typing,
            "cpu": cpu,
            "audio": audio,
            "fullscreen": if self.hypr.is_some() { "hyprland" } else { "off" },
            "idle": if self.idle_pause.is_some() { "ext-idle-notify" } else { "off" },
        })
    }

    fn log_sources(&mut self) {
        let line = self
            .sources_json()
            .as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        if line != self.sources_line {
            log::info!("sources: {line}");
            self.sources_line = line;
        }
    }

    /// Everything that depends on which scene is loaded.
    fn scene_changed(&mut self) {
        self.uses = self
            .engine
            .bus
            .snapshot()
            .into_iter()
            .filter_map(|(n, ..)| n.split('.').next().map(String::from))
            .collect();
        self.audio_failed = false;
        self.update_sources();
        self.update_covered();
        self.schedule();
    }

    // ---- still frames ---------------------------------------------------

    /// A lock frame asked for before anything was drawn: write it now.
    fn check_still(&mut self) {
        if self.still_pending {
            self.save_still();
        }
    }

    /// The lock frame: what's on screen right now, per output, written
    /// atomically to `$XDG_RUNTIME_DIR/ukiyo/still-<output>.png`. Taken the
    /// moment `still` is asked for, since a lock screen covers the
    /// wallpaper at once.
    fn save_still(&mut self) {
        self.still_pending = false;
        let Some(dir) = runtime_dir() else { return };
        if self.last_frame.is_none() {
            self.still_pending = true;
            return;
        }
        match self.capture_all(&dir, |name| format!("still-{name}.png")) {
            Ok(paths) if !paths.is_empty() => {
                self.still_serial += 1;
                log::info!("lock frames ({}): {paths:?}", self.still_serial);
                self.still_frames = paths;
            }
            Ok(_) => self.still_pending = true,
            Err(e) => log::warn!("lock frame: {e:#}"),
        }
    }

    /// Capture every drawn output to `dir/<name(output)>`.
    fn capture_all(&mut self, dir: &Path, name: impl Fn(&str) -> String) -> Result<Vec<PathBuf>> {
        std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
        let frame = match &self.last_frame {
            Some(f) => f.clone(),
            None => self.engine.tick(self.now()),
        };
        let dev = self.gfx.dev.as_ref().context("no GPU yet")?;
        let mut out = Vec::new();
        for o in &mut self.outputs {
            let Some(r) = o.gpu.as_mut().and_then(|g| g.renderer.as_mut()) else {
                continue;
            };
            let img = r.capture(&dev.device, &dev.queue, &frame)?;
            let path = dir.join(name(&safe_name(&o.name)));
            write_png(&path, &img)?;
            out.push(path);
        }
        Ok(out)
    }

    // ---- config ---------------------------------------------------------

    fn warn_config(&mut self) {
        let sch = config::schema(Some((&self.engine.scene.id, &self.engine.scene.manifest)));
        for w in self.config.check(&sch) {
            if self.warned.insert(w.clone()) {
                log::warn!("{w}");
            }
        }
    }

    /// Re-read the config files and apply them.
    fn reload_config(&mut self) {
        let (cfg, warnings) = Config::load(&self.paths);
        for w in warnings {
            if self.warned.insert(w.clone()) {
                log::warn!("{w}");
            }
        }
        self.stamps = stamps(&self.paths);
        let old_scene = self.engine.settings().scene.clone();
        let old_audio = self.engine.settings().audio_device.clone();
        let (old_fps, old_scale) = self.engine.fps_and_scale();
        self.config = cfg;
        self.engine.apply_config(&self.config);
        self.warn_config();
        let new_scene = self.engine.settings().scene.clone();
        if new_scene != old_scene
            && let Some(id) = new_scene
            && id != self.engine.scene.id
            && let Err(e) = self.set_scene(&id, false)
        {
            log::warn!("config: scene `{id}`: {e}");
        }
        if self.engine.fps_and_scale() != (old_fps, old_scale) {
            self.rescale_all();
        }
        self.audio_failed = false;
        if self.engine.settings().audio_device != old_audio {
            self.audio = None;
        }
        // Sources may have been switched on or off.
        self.update_sources();
        self.update_covered();
        for o in &mut self.outputs {
            o.needs_draw = true;
        }
        self.schedule();
    }

    fn housekeeping(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        if stamps(&self.paths) != self.stamps {
            log::info!("config changed; reloading");
            self.reload_config();
        }
        if self.ticks.is_multiple_of(BATTERY_TICKS) {
            let on = power::on_battery_now();
            if on != self.on_battery {
                log::info!("{}", if on { "on battery" } else { "on AC power" });
                self.on_battery = on;
                self.engine.set_on_battery(on);
                self.rescale_all();
                self.update_sources();
            }
        }
        if self.ticks.is_multiple_of(HELPER_PROBE_TICKS)
            && self.typing_mode == TypingMode::Idle
            && self.engine.settings().typing_source == "auto"
        {
            self.update_sources();
        }
        if self.helper.as_ref().is_some_and(|h| h.finished()) {
            log::info!("typing helper went away");
            self.update_sources();
        } else if self.typing_mode == TypingMode::Helper && self.helper.is_none() && self.active() {
            // Try to reconnect.
            self.update_sources();
        }
        self.schedule();
    }

    // ---- scenes ---------------------------------------------------------

    /// Switch to a scene by id or directory. `persist`: remember it in the
    /// user config.
    fn set_scene(&mut self, arg: &str, persist: bool) -> Result<Json, String> {
        let dirs = catalog::search_path();
        let dir = catalog::find(&dirs, arg)
            .ok_or_else(|| format!("no scene `{arg}` (see `ukiyo scenes`)"))?;
        let scene = Scene::load(&dir).map_err(|e| e.to_string())?;
        let mut engine = new_engine(scene, &self.config);
        engine.set_on_battery(self.on_battery);
        engine.set_still(self.want_still);
        if let Some(id) = self.workspace_id
            && self.signal_on("workspace")
        {
            engine.bus.set("workspace.id", id as f32);
            engine.bus.set("workspace.pan", hyprland::workspace_pan(id));
        }
        if let Some(dev) = &self.gfx.dev {
            let gpu = ukiyo_core::render::SceneGpu::load(
                &dev.device,
                &dev.queue,
                &engine.scene,
                &engine.layout,
            )
            .map_err(|e| format!("{e:#}"))?;
            let scale = engine.fps_and_scale().1;
            for o in &mut self.outputs {
                if let Some(g) = &mut o.gpu
                    && let Err(e) = g.rebuild(dev, &gpu, scale)
                {
                    return Err(format!("output {}: {e:#}", o.name));
                }
                o.needs_draw = true;
            }
            self.gfx.scene = Some(gpu);
        }
        self.engine = engine;
        self.last_frame = None;
        let id = self.engine.scene.id.clone();
        log::info!("scene {} ({id})", self.engine.scene.manifest.scene.name);
        // Only an installed scene can be remembered by id.
        if persist && catalog::find(&dirs, &id).is_some_and(|d| same_dir(&d, &dir)) {
            if let Err(e) =
                config::edit::set(&self.paths.user, "scene", &toml::Value::String(id.clone()))
            {
                log::warn!("saving the scene: {e}");
            }
            let (cfg, _) = Config::load(&self.paths);
            self.config = cfg;
            self.engine.apply_config(&self.config);
            self.stamps = stamps(&self.paths);
        }
        self.scene_changed();
        Ok(self.scene_json())
    }

    fn scene_json(&self) -> Json {
        let s = &self.engine.scene;
        json!({
            "id": s.id,
            "name": s.manifest.scene.name,
            "dir": s.dir,
            "poster": s.poster_path(),
        })
    }

    // ---- IPC ------------------------------------------------------------

    pub fn handle(&mut self, req: Request) -> Response {
        let r = self.handle_inner(req);
        self.schedule();
        match r {
            Ok(v) => Response::ok(v),
            Err(e) => Response::err(e),
        }
    }

    fn current(&self) -> (&str, &ukiyo_core::scene::Manifest) {
        (&self.engine.scene.id, &self.engine.scene.manifest)
    }

    fn handle_inner(&mut self, req: Request) -> Result<Json, String> {
        match req {
            Request::Set { scene } => self.set_scene(&scene, true),
            Request::Status => Ok(self.status()),
            Request::Scenes => {
                let list = catalog::list(&catalog::search_path());
                let cur = self.engine.scene.id.clone();
                Ok(Json::Array(
                    list.into_iter()
                        .map(|e| {
                            let mut j = serde_json::to_value(&e).unwrap_or(Json::Null);
                            j["current"] = json!(e.id == cur);
                            j
                        })
                        .collect(),
                ))
            }
            Request::Signal { name, value } => {
                let name = signals::normalize(&name);
                if !signals::valid_name(&name) || !name.contains('.') {
                    return Err(format!("`{name}` isn't a signal name"));
                }
                if !value.is_finite() {
                    return Err("the value must be a finite number".into());
                }
                self.engine.bus.set(&name, value);
                self.engine.poke();
                Ok(Json::Null)
            }
            Request::Pulse { name } => {
                let name = signals::normalize(&name);
                let base = name.strip_prefix("pulse.").unwrap_or(&name).to_string();
                if !signals::valid_name(&format!("pulse.{base}")) {
                    return Err(format!("`{name}` isn't a pulse name"));
                }
                self.engine.bus.pulse(&base);
                self.engine.poke();
                Ok(Json::Null)
            }
            Request::Command {
                command,
                status,
                duration,
            } => {
                let build = self.engine.settings().is_build(&command, duration);
                if build {
                    let p = if status == 0 {
                        "build_passed"
                    } else {
                        "build_failed"
                    };
                    log::debug!("command `{command}` ({status}, {duration:.1}s): {p}");
                    self.engine.bus.pulse(p);
                    self.engine.poke();
                }
                Ok(json!({ "build": build }))
            }
            Request::Pause { reason } => {
                let r = reason
                    .filter(|r| !r.trim().is_empty())
                    .unwrap_or_else(|| "user".into());
                self.set_reason(&r, true);
                Ok(Json::Null)
            }
            Request::Resume { reason } => {
                let r = reason
                    .filter(|r| !r.trim().is_empty())
                    .unwrap_or_else(|| "user".into());
                self.set_reason(&r, false);
                if self.want_still {
                    self.want_still = false;
                    self.engine.set_still(false);
                    self.update_sources();
                }
                Ok(Json::Null)
            }
            Request::Still => {
                if self.engine.settings().lock_snapshot {
                    self.save_still();
                }
                self.want_still = true;
                self.engine.set_still(true);
                self.update_sources();
                Ok(Json::Null)
            }
            Request::Snapshot { dir } => {
                let dir = dir
                    .or_else(runtime_dir)
                    .ok_or("no directory given and XDG_RUNTIME_DIR isn't set")?;
                let stamp = chrono_stamp();
                let paths = self
                    .capture_all(&dir, |n| format!("ukiyo-{n}-{stamp}.png"))
                    .map_err(|e| format!("{e:#}"))?;
                if paths.is_empty() {
                    return Err("no output has been drawn yet".into());
                }
                Ok(json!(paths))
            }
            Request::ConfigGet { key } => confops::get(&self.config, &key, Some(self.current())),
            Request::ConfigSet { key, value } => {
                if key == "scene" {
                    let id = value
                        .as_str()
                        .ok_or("`scene`: expected a scene id")?
                        .to_string();
                    return self.set_scene(&id, true);
                }
                let v = confops::set(&self.paths, &key, &value, Some(self.current()))?;
                self.reload_config();
                Ok(json!({ "key": key, "value": config::to_json(&v) }))
            }
            Request::ConfigReset { key } => {
                let removed = confops::reset(&self.paths, &key, Some(self.current()))?;
                self.reload_config();
                Ok(json!({ "removed": removed }))
            }
            Request::ConfigSchema { scene } => match scene {
                Some(id) if id != self.engine.scene.id => {
                    let m = confops::load_manifest(&id)?;
                    Ok(confops::schema_json(&self.config, Some((&id, &m))))
                }
                _ => Ok(confops::schema_json(&self.config, Some(self.current()))),
            },
            Request::Reload => {
                self.reload_config();
                Ok(Json::Null)
            }
            Request::Quit => {
                self.exit = true;
                Ok(Json::Null)
            }
        }
    }

    fn status(&self) -> Json {
        let now = self.now();
        let state = if !self.reasons.is_empty() {
            "paused"
        } else if self.engine.target_still() {
            if self.engine.still() >= 1.0 {
                "still"
            } else {
                "stilling"
            }
        } else {
            "running"
        };
        let outputs: Vec<Json> = self
            .outputs
            .iter()
            .map(|o| {
                let b = o.buffer_size();
                json!({
                    "name": o.name,
                    "logical": [o.logical.0, o.logical.1],
                    "scale": o.scale(),
                    "buffer": [b.0, b.1],
                    "fractional": o.viewport.is_some(),
                    "fps": o.fps.get(now),
                    "covered": o.covered,
                    "configured": o.configured,
                })
            })
            .collect();
        let signals: BTreeMap<String, Json> = self
            .engine
            .bus
            .snapshot()
            .into_iter()
            .map(|(n, raw, v)| (n, json!({ "raw": raw, "value": v })))
            .collect();
        let bindings: BTreeMap<String, f32> = self.engine.binding_values().into_iter().collect();
        let (fps_cap, scale) = self.engine.fps_and_scale();
        json!({
            "scene": self.scene_json(),
            "state": state,
            "reasons": self.reasons,
            "still": self.engine.still(),
            "on_battery": self.on_battery,
            "fps_cap": fps_cap,
            "render_scale": scale,
            "outputs": outputs,
            "still_frames": self.still_frames.iter().filter(|p| p.exists()).collect::<Vec<_>>(),
            "still_serial": self.still_serial,
            "sources": self.sources_json(),
            "signals": signals,
            "bindings": bindings,
        })
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Local time as `YYYYmmdd-HHMMSS` for snapshot names.
fn chrono_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days since the epoch to a civil date (UTC; good enough for a name).
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil(days);
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_743), (2026, 10, 17));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    #[test]
    fn names() {
        assert_eq!(safe_name("eDP-1"), "eDP-1");
        assert_eq!(safe_name("HEADLESS 1/x"), "HEADLESS_1_x");
    }
}
