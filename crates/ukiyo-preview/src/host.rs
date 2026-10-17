//! A scene on a device: engine, shared GPU resources and a renderer, with
//! reload when any file in the scene's directory changes.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use ukiyo_core::config::Config;
use ukiyo_core::render::{Renderer, SceneGpu};
use ukiyo_core::{Engine, Scene};

pub struct Host {
    pub dir: PathBuf,
    pub engine: Engine,
    pub renderer: Renderer,
    pub config: Config,
    stamp: Option<SystemTime>,
    /// The scale from the engine is multiplied by this (preview override).
    pub scale_override: Option<f32>,
}

/// Newest mtime among the scene directory's files.
pub fn stamp(dir: &Path) -> Option<SystemTime> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
}

impl Host {
    pub fn load(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        dir: &Path,
        config: Config,
        format: wgpu::TextureFormat,
        size: (u32, u32),
        scale_override: Option<f32>,
    ) -> Result<Host> {
        let scene = Scene::load(dir)?;
        let engine = Engine::new(scene, &config);
        for u in &engine.unknown_names {
            log::warn!("binding reads an unknown name: {u}");
        }
        let gpu = SceneGpu::load(device, queue, &engine.scene, &engine.layout)?;
        let scale = scale_override.unwrap_or(engine.fps_and_scale().1);
        let renderer = Renderer::new(device, gpu, format, size, scale)?;
        Ok(Host {
            dir: dir.to_path_buf(),
            engine,
            renderer,
            config,
            stamp: stamp(dir),
            scale_override,
        })
    }

    /// Reload if the scene's files changed. Keeps signal values. Returns
    /// an error message to show (the old scene keeps running) on failure.
    pub fn maybe_reload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Option<Result<(), String>> {
        let now = stamp(&self.dir);
        if now == self.stamp {
            return None;
        }
        self.stamp = now;
        let format = self.renderer.format();
        let size = self.renderer.size();
        let signals = self.engine.bus.snapshot();
        let still = self.engine.target_still();
        let r = Host::load(
            device,
            queue,
            &self.dir,
            self.config.clone(),
            format,
            size,
            self.scale_override,
        )
        .with_context(|| format!("reloading {}", self.dir.display()));
        Some(match r {
            Ok(mut h) => {
                for (name, raw, _) in signals {
                    h.engine.bus.set(&name, raw);
                }
                h.engine.set_still(still);
                *self = h;
                Ok(())
            }
            Err(e) => Err(format!("{e:#}")),
        })
    }

    pub fn resize(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        let scale = self.scale_override.unwrap_or(self.engine.fps_and_scale().1);
        self.renderer.resize(device, size, scale);
    }

    pub fn set_config(&mut self, device: &wgpu::Device, config: Config) {
        self.engine.apply_config(&config);
        self.config = config;
        let size = self.renderer.size();
        self.resize(device, size);
    }
}
