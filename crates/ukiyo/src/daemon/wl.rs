//! The Wayland side: a background layer surface per output, HiDPI
//! (fractional scale + viewporter, else integer buffer scale), frame
//! callbacks, the pointer fallback for the cursor, idle notifications.

use smithay_client_toolkit::reexports::client::{
    Connection, Dispatch, Proxy, QueueHandle,
    protocol::{wl_output, wl_pointer, wl_seat, wl_surface},
};
use smithay_client_toolkit::{
    compositor::CompositorHandler,
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use super::State;
use super::gfx::OutGpu;

/// Frames-per-second meter over the last second.
#[derive(Default)]
pub struct Fps {
    times: std::collections::VecDeque<f64>,
}

impl Fps {
    pub fn tick(&mut self, now: f64) {
        self.times.push_back(now);
        while self.times.front().is_some_and(|t| now - t > 1.0) {
            self.times.pop_front();
        }
    }

    pub fn get(&self, now: f64) -> f64 {
        self.times.iter().filter(|t| now - **t <= 1.0).count() as f64
    }
}

pub struct Output {
    /// GPU side first: dropped before the Wayland surface it draws on.
    pub gpu: Option<OutGpu>,
    pub layer: LayerSurface,
    pub wl_output: wl_output::WlOutput,
    pub name: String,
    pub viewport: Option<WpViewport>,
    pub fractional: Option<WpFractionalScaleV1>,
    /// Size in surface-local (logical) coordinates, from configure.
    pub logical: (u32, u32),
    /// Scale × 120 (the fractional-scale protocol's unit).
    pub scale120: u32,
    pub configured: bool,
    /// A frame callback is outstanding: don't draw until it's done.
    pub frame_pending: bool,
    /// Draw once even if nothing animates (new size, new scene).
    pub needs_draw: bool,
    /// A fullscreen window covers it.
    pub covered: bool,
    pub fps: Fps,
}

impl Output {
    /// Buffer size in pixels: logical × scale, rounded (the fractional-scale
    /// rule), or logical × integer scale.
    pub fn buffer_size(&self) -> (u32, u32) {
        buffer_size(self.logical, self.scale120)
    }

    pub fn scale(&self) -> f64 {
        f64::from(self.scale120) / 120.0
    }

    /// Ready for a new frame: configured, has a renderer, no callback out.
    pub fn ready(&self) -> bool {
        self.configured
            && !self.frame_pending
            && self.gpu.as_ref().is_some_and(|g| g.renderer.is_some())
    }

    /// Tell the compositor how our buffer maps to the surface.
    pub fn apply_scale(&self) {
        let surface = self.layer.wl_surface();
        match &self.viewport {
            Some(vp) => {
                surface.set_buffer_scale(1);
                vp.set_destination(self.logical.0 as i32, self.logical.1 as i32);
            }
            None => surface.set_buffer_scale((self.scale120 / 120).max(1) as i32),
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.gpu = None;
        if let Some(f) = self.fractional.take() {
            f.destroy();
        }
        if let Some(v) = self.viewport.take() {
            v.destroy();
        }
    }
}

pub fn buffer_size(logical: (u32, u32), scale120: u32) -> (u32, u32) {
    let s = u64::from(scale120.max(1));
    let f = |v: u32| ((u64::from(v) * s + 60) / 120).max(1) as u32;
    (f(logical.0), f(logical.1))
}

/// The protocol objects that have no events.
pub struct NoEvents;

macro_rules! no_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, NoEvents> for State {
            fn event(_: &mut State, _: &$t, _: <$t as Proxy>::Event, _: &NoEvents, _: &Connection, _: &QueueHandle<State>) {}
        }
    )*};
}
no_events!(
    WpViewporter,
    WpViewport,
    WpFractionalScaleManagerV1,
    ExtIdleNotifierV1
);

/// User data of a fractional-scale object: its surface.
pub struct FracData(pub wl_surface::WlSurface);

impl Dispatch<WpFractionalScaleV1, FracData> for State {
    fn event(
        state: &mut State,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        data: &FracData,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            state.set_scale(&data.0, scale);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleKind {
    /// `performance.idle_timeout`: pause.
    Pause,
    /// The short one behind the `idle` typing source.
    Typing,
}

impl Dispatch<ExtIdleNotificationV1, IdleKind> for State {
    fn event(
        state: &mut State,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        kind: &IdleKind,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let idle = matches!(event, ext_idle_notification_v1::Event::Idled);
        state.on_idle(*kind, idle);
    }
}

impl State {
    pub fn add_output(&mut self, qh: &QueueHandle<State>, output: wl_output::WlOutput) {
        let info = self.output_state.info(&output);
        let name = info
            .as_ref()
            .and_then(|i| i.name.clone())
            .unwrap_or_else(|| format!("output-{}", output.id().protocol_id()));
        let int_scale = info.as_ref().map_or(1, |i| i.scale_factor.max(1)) as u32;
        let logical = info
            .as_ref()
            .and_then(|i| i.logical_size)
            .map_or((0, 0), |(w, h)| (w.max(0) as u32, h.max(0) as u32));
        let surface = self.compositor.create_surface(qh);
        let (viewport, fractional) = match (&self.viewporter, &self.fractional_mgr) {
            (Some(vp), Some(fm)) => (
                Some(vp.get_viewport(&surface, qh, NoEvents)),
                Some(fm.get_fractional_scale(&surface, qh, FracData(surface.clone()))),
            ),
            _ => (None, None),
        };
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Background,
            Some("ukiyo"),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(0, 0);
        layer.commit();
        log::info!(
            "output {name}: {}x{} logical, scale {int_scale}{}",
            logical.0,
            logical.1,
            if viewport.is_some() {
                " (fractional scaling available)"
            } else {
                ""
            }
        );
        self.outputs.push(Output {
            gpu: None,
            layer,
            wl_output: output,
            name,
            viewport,
            fractional,
            logical,
            scale120: int_scale * 120,
            configured: false,
            frame_pending: false,
            needs_draw: true,
            covered: false,
            fps: Fps::default(),
        });
        self.update_covered();
    }

    fn output_index_by_surface(&self, s: &wl_surface::WlSurface) -> Option<usize> {
        self.outputs.iter().position(|o| o.layer.wl_surface() == s)
    }

    /// Fractional scale (×120) from the compositor.
    pub fn set_scale(&mut self, surface: &wl_surface::WlSurface, scale120: u32) {
        if let Some(i) = self.output_index_by_surface(surface)
            && self.outputs[i].scale120 != scale120
        {
            self.outputs[i].scale120 = scale120;
            log::info!(
                "output {}: scale {}",
                self.outputs[i].name,
                self.outputs[i].scale()
            );
            self.reconfigure(i);
            self.schedule();
        }
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        // With fractional scaling the preferred_scale event is authoritative.
        if let Some(i) = self.output_index_by_surface(surface)
            && self.outputs[i].viewport.is_none()
        {
            self.set_scale(surface, factor.max(1) as u32 * 120);
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(i) = self.output_index_by_surface(surface) {
            self.outputs[i].frame_pending = false;
        }
        self.schedule();
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, output: wl_output::WlOutput) {
        self.add_output(qh, output);
    }

    fn update_output(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        let Some(info) = self.output_state.info(&output) else {
            return;
        };
        if let Some(i) = self.outputs.iter().position(|o| o.wl_output == output) {
            if let Some(n) = info.name {
                self.outputs[i].name = n;
            }
            if self.outputs[i].viewport.is_none() {
                let s = info.scale_factor.max(1) as u32 * 120;
                if s != self.outputs[i].scale120 {
                    self.outputs[i].scale120 = s;
                    self.reconfigure(i);
                }
            }
        }
        self.update_covered();
        self.schedule();
    }

    fn output_destroyed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(i) = self.outputs.iter().position(|o| o.wl_output == output) {
            let o = self.outputs.remove(i);
            log::info!("output {} gone", o.name);
        }
        self.update_covered();
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if let Some(i) = self.outputs.iter().position(|o| &o.layer == layer) {
            let o = self.outputs.remove(i);
            log::info!("output {}: surface closed by the compositor", o.name);
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(i) = self.outputs.iter().position(|o| &o.layer == layer) else {
            return;
        };
        let (w, h) = configure.new_size;
        let o = &mut self.outputs[i];
        if w > 0 && h > 0 {
            o.logical = (w, h);
        }
        if o.logical.0 == 0 || o.logical.1 == 0 {
            // We asked for "the whole output" and got no size; nothing sane
            // to draw yet.
            log::warn!("output {}: configured without a size", o.name);
            return;
        }
        o.configured = true;
        self.reconfigure(i);
        self.schedule();
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.seat.is_none() {
            self.seat = Some(seat);
            self.update_idle(qh);
        }
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            self.pointer = self.seat_state.get_pointer(qh, &seat).ok();
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer
            && let Some(p) = self.pointer.take()
            && p.version() >= 3
        {
            p.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        if self.seat.as_ref() == Some(&seat) {
            self.seat = None;
            self.update_idle(qh);
        }
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for e in events {
            if let PointerEventKind::Motion { .. } | PointerEventKind::Enter { .. } = e.kind
                && let Some(i) = self.output_index_by_surface(&e.surface)
            {
                let (w, h) = self.outputs[i].logical;
                let x = (e.position.0 / f64::from(w.max(1))) * 2.0 - 1.0;
                let y = (e.position.1 / f64::from(h.max(1))) * 2.0 - 1.0;
                self.on_pointer(x as f32, y as f32);
            }
        }
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(State);
smithay_client_toolkit::delegate_dispatch2!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidpi_buffer_sizes() {
        // The 2880x1800 panel at scale 2: pixel-exact.
        assert_eq!(buffer_size((1440, 900), 240), (2880, 1800));
        // 1.25 on a 2560x1600 panel: 2048x1280 logical.
        assert_eq!(buffer_size((2048, 1280), 150), (2560, 1600));
        // 1.5 with rounding: 1707x960 logical on 2560x1440.
        assert_eq!(buffer_size((1707, 960), 180), (2561, 1440));
        assert_eq!(buffer_size((1920, 1080), 120), (1920, 1080));
        assert_eq!(buffer_size((0, 0), 120), (1, 1));
    }

    #[test]
    fn fps_meter() {
        let mut f = Fps::default();
        for i in 0..90 {
            f.tick(f64::from(i) / 30.0);
        }
        assert!((30.0..=31.0).contains(&f.get(89.0 / 30.0)));
        assert_eq!(f.get(10.0), 0.0);
    }
}
