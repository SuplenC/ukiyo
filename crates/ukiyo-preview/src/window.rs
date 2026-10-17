//! The interactive preview: the scene in a window, a panel to fake every
//! signal and to tune every option (generated from the config schema), and
//! live reload of the scene's files.
//!
//! Keys: type anything to drive `typing.rate` from your own keystrokes.
//! Ctrl+F build failed, Ctrl+P build passed, Ctrl+M music, Ctrl+L lock
//! (still), Ctrl+B battery, Ctrl+1..9 workspace, Ctrl+H hide the panel,
//! Ctrl+S save the tuned options to the user config, Esc quit.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use toml::Value;
use ukiyo_core::Scene;
use ukiyo_core::config::{self, Config, Kind, Layer, OptDef};
use ukiyo_core::render::open_device;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::host::Host;
use crate::{Args, build_config, parse_signals};

pub fn run(args: Args) -> Result<()> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        args,
        state: None,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    if let Some(e) = app.error {
        return Err(e);
    }
    Ok(())
}

struct App {
    args: Args,
    state: Option<State>,
    error: Option<anyhow::Error>,
}

/// Fake signals the panel controls.
struct Fake {
    mouse_cursor: bool,
    cursor: [f32; 2],
    typing_from_keys: bool,
    typing: f32,
    keys: VecDeque<f64>,
    cpu: f32,
    music: bool,
    bpm: f32,
    music_since: f64,
    workspace: u32,
    follow_clock: bool,
    hour: f32,
    still: bool,
    battery: bool,
}

struct State {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface_config: wgpu::SurfaceConfiguration,
    view_format: wgpu::TextureFormat,
    host: Host,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    start: Instant,
    last_frame: Instant,
    fps: f32,
    fake: Fake,
    /// Option overrides set in the panel (key -> value).
    overrides: toml::Table,
    base_texts: Vec<(Layer, String)>,
    schema: Vec<OptDef>,
    panel: bool,
    message: Option<String>,
    modifiers: ModifiersState,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match State::new(el, &self.args) {
            Ok(s) => self.state = Some(s),
            Err(e) => {
                self.error = Some(e);
                el.exit();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(s) = &mut self.state else { return };
        let consumed = s.egui_state.on_window_event(&s.window, &event).consumed;
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(size) => s.resize(size.width, size.height),
            WindowEvent::ModifiersChanged(m) => s.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                let size = s.window.inner_size();
                if s.fake.mouse_cursor {
                    s.fake.cursor = [
                        (position.x as f32 / size.width.max(1) as f32) * 2.0 - 1.0,
                        (position.y as f32 / size.height.max(1) as f32) * 2.0 - 1.0,
                    ];
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                if event.logical_key == Key::Named(NamedKey::Escape) {
                    el.exit();
                    return;
                }
                if s.modifiers.control_key() || s.modifiers.super_key() {
                    s.shortcut(&event.logical_key);
                } else if !consumed && s.fake.typing_from_keys {
                    s.fake.keys.push_back(s.start.elapsed().as_secs_f64());
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = s.frame() {
                    self.error = Some(e);
                    el.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _el: &ActiveEventLoop) {
        if let Some(s) = &self.state {
            s.window.request_redraw();
        }
    }
}

impl State {
    fn new(el: &ActiveEventLoop, args: &Args) -> Result<State> {
        let scene = Scene::load(&args.scene)?;
        let (w, h) = args.size.unwrap_or((1440, 900));
        let window = Arc::new(
            el.create_window(
                Window::default_attributes()
                    .with_title(format!("ukiyo preview: {}", scene.manifest.scene.name))
                    .with_inner_size(winit::dpi::LogicalSize::new(w, h)),
            )?,
        );
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle(
            Box::new(el.owned_display_handle()),
        ));
        let surface = instance.create_surface(window.clone())?;
        let (adapter, device, queue) = pollster::block_on(open_device(&instance, Some(&surface)))?;
        let size = window.inner_size();
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let view_format = format.add_srgb_suffix();
        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![view_format, format.remove_srgb_suffix()],
        };
        surface.configure(&device, &surface_config);

        let mut base_texts = Vec::new();
        if args.user_config {
            let p = config::Paths::from_env();
            for path in p.system.iter().chain([&p.defaults, &p.user]) {
                if let Ok(t) = std::fs::read_to_string(path) {
                    base_texts.push((Layer::Defaults, t));
                }
            }
        }
        for p in &args.configs {
            base_texts.push((
                Layer::Defaults,
                std::fs::read_to_string(p).with_context(|| p.display().to_string())?,
            ));
        }
        let config = build_config(args, &scene)?;
        let schema = config::schema(Some((&scene.id, &scene.manifest)));
        let mut host = Host::load(
            &device,
            &queue,
            &args.scene,
            config,
            view_format,
            (surface_config.width, surface_config.height),
            args.scale,
        )?;
        for (k, v) in parse_signals(args)? {
            host.engine.bus.set(&k, v);
        }
        host.engine.set_still(args.still);

        let egui_ctx = egui::Context::default();
        egui_ctx.set_visuals(egui::Visuals::dark());
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            &device,
            format.remove_srgb_suffix(),
            egui_wgpu::RendererOptions::default(),
        );

        let now = Instant::now();
        Ok(State {
            window,
            surface,
            device,
            queue,
            surface_config,
            view_format,
            host,
            egui_ctx,
            egui_state,
            egui_renderer,
            start: now,
            last_frame: now,
            fps: 0.0,
            fake: Fake {
                mouse_cursor: true,
                cursor: [0.0, 0.0],
                typing_from_keys: true,
                typing: 0.0,
                keys: VecDeque::new(),
                cpu: 0.05,
                music: false,
                bpm: 118.0,
                music_since: 0.0,
                workspace: 1,
                follow_clock: args.hour.is_none(),
                hour: args.hour.unwrap_or(22.0),
                still: args.still,
                battery: false,
            },
            overrides: toml::Table::new(),
            base_texts,
            schema,
            panel: true,
            message: None,
            modifiers: ModifiersState::empty(),
        })
    }

    fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.surface_config.width = w;
        self.surface_config.height = h;
        self.surface.configure(&self.device, &self.surface_config);
        self.host.resize(&self.device, (w, h));
    }

    fn shortcut(&mut self, key: &Key) {
        let Key::Character(c) = key else { return };
        match c.to_ascii_lowercase().as_str() {
            "f" => self.host.engine.bus.pulse("build_failed"),
            "p" => self.host.engine.bus.pulse("build_passed"),
            "m" => self.toggle_music(),
            "l" => self.fake.still = !self.fake.still,
            "b" => self.fake.battery = !self.fake.battery,
            "h" => self.panel = !self.panel,
            "s" => self.save(),
            d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() && d != "0" => {
                self.fake.workspace = u32::from(d.as_bytes()[0] - b'0');
            }
            _ => {}
        }
    }

    fn toggle_music(&mut self) {
        self.fake.music = !self.fake.music;
        self.fake.music_since = self.start.elapsed().as_secs_f64();
    }

    /// Write the panel's overrides into the user's config file.
    fn save(&mut self) {
        let path = config::Paths::from_env().user;
        let mut flat = Vec::new();
        flatten("", &self.overrides, &mut flat);
        let mut result = Ok(());
        for (k, v) in &flat {
            if let Err(e) = config::edit::set(&path, k, v) {
                result = Err(e);
            }
        }
        self.message = Some(match result {
            Ok(()) => format!("saved {} option(s) to {}", flat.len(), path.display()),
            Err(e) => format!("save failed: {e}"),
        });
    }

    fn apply_overrides(&mut self) {
        let mut texts: Vec<(Layer, String)> = self.base_texts.clone();
        texts.push((
            Layer::User,
            toml::to_string(&self.overrides).unwrap_or_default(),
        ));
        let refs: Vec<(Layer, &str)> = texts.iter().map(|(l, t)| (*l, t.as_str())).collect();
        match Config::from_layers(&refs) {
            Ok(cfg) => self.host.set_config(&self.device, cfg),
            Err(e) => self.message = Some(e),
        }
    }

    fn feed_signals(&mut self, t: f64) {
        let f = &mut self.fake;
        // Keystrokes in the last 1.5 s, as a rate: 8 keys/s = 1.
        while f.keys.front().is_some_and(|k| t - k > 1.5) {
            f.keys.pop_front();
        }
        let from_keys = (f.keys.len() as f32 / 1.5 / 8.0).min(1.0);
        let typing = if f.typing_from_keys {
            from_keys.max(f.typing)
        } else {
            f.typing
        };
        let bus = &mut self.host.engine.bus;
        bus.set("cursor.x", f.cursor[0]);
        bus.set("cursor.y", f.cursor[1]);
        bus.set("typing.rate", typing);
        bus.set("cpu.load", f.cpu);
        bus.set("workspace.id", f.workspace as f32);
        bus.set(
            "workspace.pan",
            ((f.workspace as f32 - 1.0) / 8.0).clamp(0.0, 1.0) * 2.0 - 1.0,
        );
        let (level, bass, mid, treble, beat) = if f.music {
            crate::script::fake_music(f.bpm, t - f.music_since)
        } else {
            (0.0, 0.0, 0.0, 0.0, 0.0)
        };
        bus.set("audio.level", level);
        bus.set("audio.bass", bass);
        bus.set("audio.mid", mid);
        bus.set("audio.treble", treble);
        bus.set("audio.beat", beat);
        self.host
            .engine
            .set_clock_override((!f.follow_clock).then_some(f.hour));
        self.host.engine.set_still(f.still);
        self.host.engine.set_on_battery(f.battery);
    }

    fn frame(&mut self) -> Result<()> {
        if let Some(r) = self.host.maybe_reload(&self.device, &self.queue) {
            self.message = Some(match r {
                Ok(()) => "scene reloaded".into(),
                Err(e) => e,
            });
            if self.message.as_deref() == Some("scene reloaded") {
                self.schema = config::schema(Some((
                    &self.host.engine.scene.id,
                    &self.host.engine.scene.manifest,
                )));
            }
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;
        self.fps = self.fps * 0.95 + (1.0 / dt.max(1e-4)) * 0.05;
        let t = self.start.elapsed().as_secs_f64();
        self.feed_signals(t);
        let frame = self.host.engine.tick(t);

        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            _ => {
                self.surface.configure(&self.device, &self.surface_config);
                return Ok(());
            }
        };
        let view = output.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.view_format),
            ..Default::default()
        });
        self.host
            .renderer
            .render(&self.device, &self.queue, &frame, &view);

        // The panel.
        let raw = self.egui_state.take_egui_input(&self.window);
        let ctx = self.egui_ctx.clone();
        let mut changed = false;
        let full = ctx.run_ui(raw, |ui| {
            if self.panel {
                changed = self.panel_ui(ui);
            }
        });
        if changed {
            self.apply_overrides();
        }
        self.egui_state
            .handle_platform_output(&self.window, full.platform_output);
        let ppp = full.pixels_per_point;
        let jobs = ctx.tessellate(full.shapes, ppp);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.surface_config.width, self.surface_config.height],
            pixels_per_point: ppp,
        };
        for (id, deltas) in &full.textures_delta.set {
            for delta in deltas {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        let ui_view = output.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.surface_config.format.remove_srgb_suffix()),
            ..Default::default()
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        let cmds =
            self.egui_renderer
                .update_buffers(&self.device, &self.queue, &mut enc, &jobs, &screen);
        {
            let rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &ui_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            let mut rp = rp.forget_lifetime();
            self.egui_renderer.render(&mut rp, &jobs, &screen);
        }
        self.queue.submit(cmds.into_iter().chain([enc.finish()]));
        for id in &full.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
        self.window.pre_present_notify();
        self.queue.present(output);
        Ok(())
    }

    /// Returns whether an option changed.
    fn panel_ui(&mut self, root: &mut egui::Ui) -> bool {
        let mut changed = false;
        egui::Panel::right("controls").resizable(true).default_size(340.0).show(root, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let e = &self.host.engine;
                let (fps_cap, scale) = e.fps_and_scale();
                let internal = self.host.renderer.internal_size();
                ui.heading(&e.scene.manifest.scene.name);
                ui.label(format!(
                    "{:.0} fps (scene cap {:.0}) · scene pass {}x{} ({:.2}x)",
                    self.fps, fps_cap, internal.0, internal.1, scale
                ));
                if let Some(m) = &self.message {
                    ui.colored_label(egui::Color32::LIGHT_YELLOW, m);
                }
                ui.small("Type to drive typing. Ctrl+F fail, Ctrl+P pass, Ctrl+M music, Ctrl+L lock, Ctrl+B battery, Ctrl+1..9 workspace, Ctrl+H hide, Ctrl+S save.");
                ui.separator();

                egui::CollapsingHeader::new("Signals").default_open(true).show(ui, |ui| {
                    let f = &mut self.fake;
                    ui.checkbox(&mut f.mouse_cursor, "Mouse is the cursor signal");
                    ui.add(egui::Slider::new(&mut f.cursor[0], -1.0..=1.0).text("cursor.x"));
                    ui.add(egui::Slider::new(&mut f.cursor[1], -1.0..=1.0).text("cursor.y"));
                    ui.checkbox(&mut f.typing_from_keys, "My keystrokes are typing");
                    ui.add(egui::Slider::new(&mut f.typing, 0.0..=1.0).text("typing.rate"));
                    ui.add(egui::Slider::new(&mut f.cpu, 0.0..=1.0).text("cpu.load"));
                    ui.add(egui::Slider::new(&mut f.workspace, 1..=9).text("workspace"));
                    ui.horizontal(|ui| {
                        if ui.button("Build failed").clicked() {
                            self.host.engine.bus.pulse("build_failed");
                        }
                        if ui.button("Build passed").clicked() {
                            self.host.engine.bus.pulse("build_passed");
                        }
                    });
                    let f = &mut self.fake;
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut f.music, "Music").changed() {
                            f.music_since = self.start.elapsed().as_secs_f64();
                        }
                        ui.add(egui::Slider::new(&mut f.bpm, 60.0..=180.0).text("bpm"));
                    });
                    ui.checkbox(&mut f.follow_clock, "Follow the real clock");
                    ui.add_enabled(!f.follow_clock, egui::Slider::new(&mut f.hour, 0.0..=24.0).text("hour"));
                    ui.checkbox(&mut f.still, "Locked (still)");
                    ui.checkbox(&mut f.battery, "On battery");
                    let bus = &self.host.engine.bus;
                    ui.small(format!(
                        "clock.dusk {:.2} · clock.night {:.2} · still {:.2}",
                        bus.get("clock.dusk"),
                        bus.get("clock.night"),
                        self.host.engine.still()
                    ));
                });

                // Every option, from the schema, grouped.
                let scene_id = self.host.engine.scene.id.clone();
                let cfg = self.host.config.clone();
                let mut groups: Vec<String> = Vec::new();
                for d in &self.schema {
                    if !groups.contains(&d.group) {
                        groups.push(d.group.clone());
                    }
                }
                // Scene groups first: those are what you tune here.
                groups.sort_by_key(|g| !g.starts_with(&self.host.engine.scene.manifest.scene.name));
                for g in groups {
                    egui::CollapsingHeader::new(&g).default_open(g.starts_with(&self.host.engine.scene.manifest.scene.name)).show(ui, |ui| {
                        for d in self.schema.iter().filter(|d| d.group == g) {
                            let (value, source) = cfg.value(d, Some(&scene_id));
                            if let Some(v) = option_ui(ui, d, &value, source.is_some()) {
                                match v {
                                    Some(v) => set_dotted(&mut self.overrides, &d.key, v),
                                    None => remove_dotted(&mut self.overrides, &d.key),
                                }
                                changed = true;
                            }
                        }
                    });
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Reset scene options").clicked() {
                        remove_dotted(&mut self.overrides, &format!("scenes.{scene_id}"));
                        changed = true;
                    }
                    if ui.button("Save to my config").clicked() {
                        self.save();
                    }
                });
            });
        });
        changed
    }
}

/// One option's widget. Returns Some(Some(v)) to set, Some(None) to reset.
fn option_ui(
    ui: &mut egui::Ui,
    d: &OptDef,
    value: &Value,
    overridden: bool,
) -> Option<Option<Value>> {
    let mut out = None;
    ui.horizontal(|ui| {
        let label = if overridden {
            format!("{} •", d.label)
        } else {
            d.label.clone()
        };
        match &d.kind {
            Kind::Bool => {
                let mut b = value.as_bool().unwrap_or(false);
                if ui.checkbox(&mut b, label).on_hover_text(&d.help).changed() {
                    out = Some(Some(Value::Boolean(b)));
                }
            }
            Kind::Float {
                min,
                max,
                step,
                unit,
            } => {
                let old = value.as_float().unwrap_or(0.0) as f32;
                let mut x = old;
                let s = egui::Slider::new(&mut x, *min..=*max)
                    .step_by(f64::from(*step))
                    .suffix(unit.as_str())
                    .text(label);
                // The slider snaps to its step on the first frame: not a change.
                if ui.add(s).on_hover_text(&d.help).changed() && (x - old).abs() > step * 0.25 {
                    out = Some(Some(Value::Float(f64::from(x))));
                }
            }
            Kind::Choice { options } => {
                let mut cur = value.as_str().unwrap_or_default().to_string();
                egui::ComboBox::from_label(label)
                    .selected_text(&cur)
                    .show_ui(ui, |ui| {
                        for o in options {
                            if ui.selectable_value(&mut cur, o.clone(), o).changed() {
                                out = Some(Some(Value::String(cur.clone())));
                            }
                        }
                    });
            }
            Kind::Color => {
                let [r, g, b] = ukiyo_core::scene::parse_color(value.as_str().unwrap_or("#000000"))
                    .unwrap_or([0.0; 3]);
                let mut c = [r, g, b];
                ui.label(label);
                if ui.color_edit_button_rgb(&mut c).changed() {
                    let hex = format!(
                        "#{:02x}{:02x}{:02x}",
                        (c[0] * 255.0) as u8,
                        (c[1] * 255.0) as u8,
                        (c[2] * 255.0) as u8
                    );
                    out = Some(Some(Value::String(hex)));
                }
            }
            Kind::String | Kind::List => {
                ui.label(format!("{label}: {value}"));
            }
        }
        if overridden
            && ui
                .small_button("reset")
                .on_hover_text("Reset to default")
                .clicked()
        {
            out = Some(None);
        }
    });
    out
}

fn set_dotted(t: &mut toml::Table, key: &str, v: Value) {
    let mut parts: Vec<&str> = key.split('.').collect();
    let last = parts.pop().unwrap_or_default();
    let mut cur = t;
    for p in parts {
        cur = cur
            .entry(p.to_string())
            .or_insert_with(|| Value::Table(toml::Table::new()))
            .as_table_mut()
            .expect("table");
    }
    cur.insert(last.to_string(), v);
}

fn remove_dotted(t: &mut toml::Table, key: &str) {
    fn go(t: &mut toml::Table, parts: &[&str]) {
        match parts {
            [last] => {
                t.remove(*last);
            }
            [first, rest @ ..] => {
                if let Some(Value::Table(sub)) = t.get_mut(*first) {
                    go(sub, rest);
                    if sub.is_empty() {
                        t.remove(*first);
                    }
                }
            }
            [] => {}
        }
    }
    go(t, &key.split('.').collect::<Vec<_>>());
}

fn flatten(prefix: &str, t: &toml::Table, out: &mut Vec<(String, Value)>) {
    for (k, v) in t {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            Value::Table(sub) => flatten(&key, sub, out),
            _ => out.push((key, v.clone())),
        }
    }
}
