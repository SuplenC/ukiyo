//! wgpu on raw Wayland surfaces: one instance and device, one swapchain and
//! [`Renderer`] per output.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, RawDisplayHandle, RawWindowHandle,
    WaylandDisplayHandle, WaylandWindowHandle,
};
use ukiyo_core::render::{Renderer, SceneGpu};
use ukiyo_core::{Engine, Frame};

/// The Wayland display, for wgpu (GLES needs it at instance creation).
#[derive(Debug)]
struct WlDisplay(NonNull<c_void>);

// The pointer is only handed to wgpu/EGL, which use it from any thread
// as libwayland allows.
unsafe impl Send for WlDisplay {}
unsafe impl Sync for WlDisplay {}

impl HasDisplayHandle for WlDisplay {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: the connection outlives the instance (the daemon drops
        // every wgpu object before the connection).
        Ok(unsafe {
            DisplayHandle::borrow_raw(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(self.0)))
        })
    }
}

pub struct Dev {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

pub struct Gfx {
    display: NonNull<c_void>,
    pub instance: wgpu::Instance,
    pub dev: Option<Dev>,
    pub scene: Option<Arc<SceneGpu>>,
}

impl Gfx {
    pub fn new(display: *mut c_void) -> Result<Gfx> {
        let display = NonNull::new(display).context("no Wayland display pointer")?;
        let desc = wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(WlDisplay(
            display,
        )));
        Ok(Gfx {
            display,
            instance: wgpu::Instance::new(desc),
            dev: None,
            scene: None,
        })
    }

    /// A wgpu surface on a `wl_surface`.
    ///
    /// # Safety
    /// `surface` must stay alive until the returned surface is dropped.
    pub unsafe fn create_surface(&self, surface: *mut c_void) -> Result<wgpu::Surface<'static>> {
        let surface = NonNull::new(surface).context("null wl_surface")?;
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                self.display,
            ))),
            raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(surface)),
        };
        // SAFETY: per this function's contract.
        unsafe { self.instance.create_surface_unsafe(target) }
            .map_err(|e| anyhow!("can't create a GPU surface: {e}"))
    }

    /// Open the device (once, compatible with the first surface) and load
    /// the scene's GPU resources on it.
    pub fn ensure_device(&mut self, surface: &wgpu::Surface<'_>, engine: &Engine) -> Result<()> {
        if self.dev.is_none() {
            let (adapter, device, queue) = pollster::block_on(ukiyo_core::render::open_device(
                &self.instance,
                Some(surface),
            ))?;
            let info = adapter.get_info();
            log::info!(
                "GPU: {} ({:?}, {:?})",
                info.name,
                info.backend,
                info.device_type
            );
            self.dev = Some(Dev {
                adapter,
                device,
                queue,
            });
        }
        if self.scene.is_none() {
            self.load_scene(engine)?;
        }
        Ok(())
    }

    /// (Re)load the scene's textures and shader. Only when a device exists.
    pub fn load_scene(&mut self, engine: &Engine) -> Result<()> {
        let Some(dev) = &self.dev else { return Ok(()) };
        let gpu = SceneGpu::load(&dev.device, &dev.queue, &engine.scene, &engine.layout)?;
        self.scene = Some(gpu);
        Ok(())
    }
}

/// The surface format and the (sRGB) view format to render through.
pub fn pick_format(
    formats: &[wgpu::TextureFormat],
) -> Option<(wgpu::TextureFormat, wgpu::TextureFormat)> {
    // 8-bit first: what the renderer is tuned for and what every compositor
    // scans out cheaply.
    let eight = |f: &wgpu::TextureFormat| {
        matches!(
            f.remove_srgb_suffix(),
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
        )
    };
    formats
        .iter()
        .filter(|f| eight(f))
        .chain(formats.iter())
        .find(|f| f.is_srgb() || f.add_srgb_suffix().is_srgb())
        .map(|f| (*f, f.add_srgb_suffix()))
}

/// Mailbox, else Immediate, else Fifo. (Fifo blocks in Mesa's WSI while a
/// surface is hidden; our pacing comes from frame callbacks anyway.)
pub fn pick_present(modes: &[wgpu::PresentMode]) -> wgpu::PresentMode {
    [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Immediate]
        .into_iter()
        .find(|m| modes.contains(m))
        .unwrap_or(wgpu::PresentMode::Fifo)
}

/// One output's swapchain and renderer.
pub struct OutGpu {
    pub renderer: Option<Renderer>,
    pub config: Option<wgpu::SurfaceConfiguration>,
    pub view_format: wgpu::TextureFormat,
    // Last: dropped after the renderer, before the wl_surface.
    pub surface: wgpu::Surface<'static>,
}

impl OutGpu {
    pub fn new(surface: wgpu::Surface<'static>) -> OutGpu {
        OutGpu {
            renderer: None,
            config: None,
            view_format: wgpu::TextureFormat::Bgra8UnormSrgb,
            surface,
        }
    }

    /// Configure the swapchain for `size` (pixels) and make sure the
    /// renderer matches it and the render `scale`.
    pub fn configure(
        &mut self,
        dev: &Dev,
        scene: &Arc<SceneGpu>,
        size: (u32, u32),
        scale: f32,
    ) -> Result<()> {
        let (w, h) = (size.0.max(1), size.1.max(1));
        let caps = self.surface.get_capabilities(&dev.adapter);
        let (format, view) = pick_format(&caps.formats)
            .context("the surface offers no 8-bit (sRGB-capable) format")?;
        let mut cfg = self
            .surface
            .get_default_config(&dev.adapter, w, h)
            .context("the surface isn't supported by this GPU")?;
        cfg.format = format;
        cfg.present_mode = pick_present(&caps.present_modes);
        cfg.alpha_mode = if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            caps.alpha_modes
                .first()
                .copied()
                .unwrap_or(wgpu::CompositeAlphaMode::Auto)
        };
        cfg.view_formats = if view != format { vec![view] } else { vec![] };
        cfg.desired_maximum_frame_latency = 2;
        if self.config.as_ref() != Some(&cfg) {
            log::debug!(
                "surface {w}x{h} {format:?} (view {view:?}) {:?}",
                cfg.present_mode
            );
            self.surface.configure(&dev.device, &cfg);
            self.config = Some(cfg);
        }
        let fresh = match &mut self.renderer {
            Some(r) if self.view_format == view => {
                r.resize(&dev.device, (w, h), scale);
                false
            }
            _ => true,
        };
        if fresh {
            self.view_format = view;
            self.renderer = Some(Renderer::new(
                &dev.device,
                scene.clone(),
                view,
                (w, h),
                scale,
            )?);
        }
        Ok(())
    }

    /// New scene: a new renderer on the same swapchain.
    pub fn rebuild(&mut self, dev: &Dev, scene: &Arc<SceneGpu>, scale: f32) -> Result<()> {
        let Some(cfg) = &self.config else {
            return Ok(());
        };
        self.renderer = Some(Renderer::new(
            &dev.device,
            scene.clone(),
            self.view_format,
            (cfg.width, cfg.height),
            scale,
        )?);
        Ok(())
    }
}

pub enum Drawn {
    /// Presented; a frame callback was requested before it.
    Yes,
    /// Nothing presented (hidden, timed out); try again later.
    Skipped,
    /// The swapchain needs configuring again.
    Reconfigure,
}

/// Draw `frame` into the output and present it. `before_present` runs
/// right before the commit (request the frame callback there).
pub fn draw(out: &mut OutGpu, dev: &Dev, frame: &Frame, before_present: impl FnOnce()) -> Drawn {
    let Some(renderer) = &mut out.renderer else {
        return Drawn::Skipped;
    };
    let tex = match out.surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(t) => t,
        wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
            // Draw this one; configure again for the next.
            out.config = None;
            t
        }
        wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
            return Drawn::Skipped;
        }
        wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
            out.config = None;
            return Drawn::Reconfigure;
        }
        other => {
            log::warn!("can't get a surface texture: {other:?}");
            return Drawn::Skipped;
        }
    };
    let view = tex.texture.create_view(&wgpu::TextureViewDescriptor {
        format: Some(out.view_format),
        ..Default::default()
    });
    renderer.render(&dev.device, &dev.queue, frame, &view);
    before_present();
    dev.queue.present(tex);
    Drawn::Yes
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::TextureFormat as F;

    #[test]
    fn formats() {
        assert_eq!(
            pick_format(&[F::Bgra8Unorm, F::Bgra8UnormSrgb]),
            Some((F::Bgra8Unorm, F::Bgra8UnormSrgb))
        );
        assert_eq!(
            pick_format(&[F::Rgb10a2Unorm, F::Rgba8UnormSrgb]),
            Some((F::Rgba8UnormSrgb, F::Rgba8UnormSrgb)),
            "8-bit preferred"
        );
        assert_eq!(pick_format(&[F::Rgb10a2Unorm]), None);
        assert_eq!(
            pick_present(&[wgpu::PresentMode::Fifo, wgpu::PresentMode::Immediate]),
            wgpu::PresentMode::Immediate
        );
        assert_eq!(pick_present(&[]), wgpu::PresentMode::Fifo);
    }
}
