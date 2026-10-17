//! The wgpu renderer.
//!
//! [`SceneGpu`] holds what every output shares: the scene's textures and
//! shader. A [`Renderer`] draws one output: the scene pass (the author's
//! `fs_scene`, into an HDR target at the internal resolution), the built-in
//! bloom chain, then the post pass (the author's `fs_post`, or a default
//! ACES tonemap) into the output. The output view must be sRGB.

use std::borrow::Cow;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use image::{DynamicImage, GenericImageView, imageops::FilterType};
use wgpu::util::DeviceExt;

use crate::engine::{Frame, UniformLayout};
use crate::scene::{ColorSpace, Scene};

const HDR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Pick an adapter (low power: the integrated GPU on a laptop) and open a
/// device with the limits that adapter has.
pub async fn open_device(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<(wgpu::Adapter, wgpu::Device, wgpu::Queue)> {
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: surface,
            ..Default::default()
        })
        .await
        .context("no GPU adapter (not even a software one)")?;
    let limits = wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("ukiyo"),
            required_limits: limits,
            ..Default::default()
        })
        .await
        .context("can't open the GPU device")?;
    Ok((adapter, device, queue))
}

struct SceneTexture {
    view: wgpu::TextureView,
    size: (u32, u32),
}

/// Scene resources shared by every output on one device.
pub struct SceneGpu {
    pub layout: UniformLayout,
    textures: Vec<(String, SceneTexture)>,
    frame_size: (u32, u32),
    focus: [f32; 2],
    overscan: f32,
    bloom: crate::scene::Bloom,
    module: wgpu::ShaderModule,
    bloom_module: wgpu::ShaderModule,
    g0: wgpu::BindGroupLayout,
    g1: wgpu::BindGroupLayout,
    g2: wgpu::BindGroupLayout,
    bloom_bgl: wgpu::BindGroupLayout,
    g1_group: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    sampler_repeat: wgpu::Sampler,
    black: wgpu::TextureView,
}

impl SceneGpu {
    /// Load every texture and compile the shader. Shader errors come back as
    /// errors (with naga's message), not panics.
    pub fn load(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        layout: &UniformLayout,
    ) -> Result<Arc<SceneGpu>> {
        let m = &scene.manifest;
        let max = device.limits().max_texture_dimension_2d;
        let mut textures = Vec::new();
        for (name, spec) in &m.textures {
            let spec = spec.full();
            let path = scene.path(&spec.path);
            let img = image::open(&path)
                .with_context(|| format!("texture `{name}`: {}", path.display()))?;
            let tex = upload(device, queue, name, img, spec.color, spec.mipmaps, max)
                .with_context(|| format!("texture `{name}`"))?;
            textures.push((name.to_string(), tex));
        }
        let frame_size = m
            .frame_texture()
            .and_then(|f| textures.iter().find(|(n, _)| n == f))
            .map_or((16, 9), |(_, t)| t.size);

        let source = shader_source(
            scene,
            layout,
            &textures.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(),
        );
        let module = compile(device, "scene", &source)
            .with_context(|| format!("shader {}", m.scene.shader))?;
        let bloom_module = compile(device, "bloom", include_str!("bloom.wgsl"))?;

        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let uniform_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let g0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ukiyo g0"),
            entries: &[uniform_entry(0), sampler_entry(1), sampler_entry(2)],
        });
        let g1_entries: Vec<_> = (0..textures.len() as u32).map(tex_entry).collect();
        let g1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ukiyo g1"),
            entries: &g1_entries,
        });
        let g2 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ukiyo g2"),
            entries: &[tex_entry(0), tex_entry(1)],
        });
        let bloom_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ukiyo bloom"),
            entries: &[tex_entry(0), sampler_entry(1), uniform_entry(2)],
        });
        let g1_bind: Vec<_> = textures
            .iter()
            .enumerate()
            .map(|(i, (_, t))| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: wgpu::BindingResource::TextureView(&t.view),
            })
            .collect();
        let g1_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ukiyo g1"),
            layout: &g1,
            entries: &g1_bind,
        });
        let mk_sampler = |mode| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("ukiyo"),
                address_mode_u: mode,
                address_mode_v: mode,
                address_mode_w: mode,
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            })
        };
        let black = device
            .create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("black"),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                wgpu::util::TextureDataOrder::LayerMajor,
                &[0, 0, 0, 255],
            )
            .create_view(&Default::default());
        Ok(Arc::new(SceneGpu {
            layout: layout.clone(),
            textures,
            frame_size,
            focus: m.view.focus,
            overscan: m.view.overscan,
            bloom: m.render.bloom.clone(),
            module,
            bloom_module,
            g0,
            g1,
            g2,
            bloom_bgl,
            g1_group,
            sampler: mk_sampler(wgpu::AddressMode::ClampToEdge),
            sampler_repeat: mk_sampler(wgpu::AddressMode::Repeat),
            black,
        }))
    }

    /// The scene's textures, in binding order, with their sizes.
    pub fn textures(&self) -> Vec<(&str, (u32, u32))> {
        self.textures
            .iter()
            .map(|(n, t)| (n.as_str(), t.size))
            .collect()
    }

    /// Screen uv -> image uv for an output of `w`×`h`: cover the output,
    /// centred on the focus point, zoomed by overscan. (scale, centre)
    pub fn cover(&self, w: u32, h: u32) -> ([f32; 2], [f32; 2]) {
        cover(self.frame_size, (w, h), self.focus, self.overscan)
    }
}

/// See [`SceneGpu::cover`].
pub fn cover(
    img: (u32, u32),
    out: (u32, u32),
    focus: [f32; 2],
    overscan: f32,
) -> ([f32; 2], [f32; 2]) {
    let ia = img.0 as f32 / img.1.max(1) as f32;
    let oa = out.0 as f32 / out.1.max(1) as f32;
    let (sx, sy) = if oa > ia {
        (1.0, ia / oa)
    } else {
        (oa / ia, 1.0)
    };
    let (sx, sy) = (sx / overscan, sy / overscan);
    let cx = focus[0].clamp(sx / 2.0, 1.0 - sx / 2.0);
    let cy = focus[1].clamp(sy / 2.0, 1.0 - sy / 2.0);
    ([sx, sy], [cx, cy])
}

fn compile(device: &wgpu::Device, label: &str, source: &str) -> Result<wgpu::ShaderModule> {
    let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(source)),
    });
    if let Some(e) = pollster_block(scope.pop()) {
        bail!("{e}");
    }
    Ok(module)
}

/// Minimal executor for wgpu's futures (they're ready immediately on native).
fn pollster_block<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

/// The generated prelude, the author's code, and a default `fs_post`.
pub fn shader_source(scene: &Scene, layout: &UniformLayout, textures: &[&str]) -> String {
    let mut s = String::from("// ---- ukiyo prelude (generated) ----\n");
    s += &layout.wgsl();
    s += "@group(0) @binding(0) var<uniform> u: Ukiyo;\n@group(0) @binding(1) var smp: sampler;\n@group(0) @binding(2) var smp_repeat: sampler;\n";
    for (i, t) in textures.iter().enumerate() {
        s += &format!("@group(1) @binding({i}) var t_{t}: texture_2d<f32>;\n");
    }
    s += PRELUDE;
    s += "// ---- scene ----\n";
    s += &scene.shader_source;
    if !scene.shader_source.contains("fn fs_post") {
        s += DEFAULT_POST;
    }
    s
}

const PRELUDE: &str = r#"
@group(2) @binding(0) var t_scene: texture_2d<f32>;
@group(2) @binding(1) var t_bloom: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // Screen uv, (0, 0) top left.
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> VsOut {
    let xy = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var o: VsOut;
    o.pos = vec4<f32>(xy * 2.0 - 1.0, 0.0, 1.0);
    o.uv = vec2<f32>(xy.x, 1.0 - xy.y);
    return o;
}

// Screen uv -> uv in the scene's frame texture (cover crop, focus, overscan).
fn image_uv(screen: vec2<f32>) -> vec2<f32> {
    return (screen - 0.5) * u.cover_scale + u.cover_center;
}

fn aces(x: vec3<f32>) -> vec3<f32> {
    let a = x * (x * 2.51 + 0.03);
    let b = x * (x * 2.43 + 0.59) + 0.14;
    return clamp(a / b, vec3(0.0), vec3(1.0));
}
"#;

const DEFAULT_POST: &str = r#"
@fragment
fn fs_post(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(t_scene, smp, in.uv).rgb + textureSample(t_bloom, smp, in.uv).rgb * 0.5;
    return vec4<f32>(aces(c), 1.0);
}
"#;

fn upload(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    name: &str,
    img: DynamicImage,
    color: ColorSpace,
    mipmaps: bool,
    max: u32,
) -> Result<SceneTexture> {
    let mut img = img;
    let (w0, h0) = img.dimensions();
    if w0.max(h0) > max {
        let k = max as f32 / w0.max(h0) as f32;
        img = img.resize_exact(
            (w0 as f32 * k) as u32,
            (h0 as f32 * k) as u32,
            FilterType::Lanczos3,
        );
    }
    let (w, h) = img.dimensions();
    // Pick the GPU format and turn the image into the matching layout.
    enum Kind {
        Rgba8,
        R8,
        R16F,
    }
    let kind = match &img {
        DynamicImage::ImageLuma8(_) => Kind::R8,
        DynamicImage::ImageLuma16(_)
        | DynamicImage::ImageLumaA16(_)
        | DynamicImage::ImageRgb16(_) => Kind::R16F,
        _ => Kind::Rgba8,
    };
    let (format, kind) = match (kind, color) {
        (Kind::Rgba8, ColorSpace::Srgb) => (wgpu::TextureFormat::Rgba8UnormSrgb, 0),
        (Kind::Rgba8, ColorSpace::Linear) => (wgpu::TextureFormat::Rgba8Unorm, 0),
        (Kind::R8, ColorSpace::Linear) => (wgpu::TextureFormat::R8Unorm, 1),
        (Kind::R8, ColorSpace::Srgb) => (wgpu::TextureFormat::Rgba8UnormSrgb, 0),
        (Kind::R16F, _) => (wgpu::TextureFormat::R16Float, 2),
    };
    let levels = if mipmaps {
        32 - w.max(h).leading_zeros()
    } else {
        1
    };
    let mut data = Vec::new();
    let mut level = img;
    for l in 0..levels {
        if l > 0 {
            let (lw, lh) = level.dimensions();
            level = level.resize_exact((lw / 2).max(1), (lh / 2).max(1), FilterType::Triangle);
        }
        match kind {
            0 => data.extend_from_slice(level.to_rgba8().as_raw()),
            1 => data.extend_from_slice(level.to_luma8().as_raw()),
            _ => {
                for v in level.to_luma16().as_raw() {
                    data.extend_from_slice(
                        &half::f16::from_f32(f32::from(*v) / 65535.0).to_le_bytes(),
                    );
                }
            }
        }
    }
    let tex = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some(name),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::MipMajor,
        &data,
    );
    Ok(SceneTexture {
        view: tex.create_view(&Default::default()),
        size: (w, h),
    })
}

struct Targets {
    hdr: wgpu::TextureView,
    bloom: Vec<(wgpu::TextureView, (u32, u32))>,
    /// Bind groups: prefilter reads hdr; down[i] reads bloom[i]; up[i] reads bloom[i + 1].
    pre: wgpu::BindGroup,
    down: Vec<wgpu::BindGroup>,
    up: Vec<wgpu::BindGroup>,
    g2: wgpu::BindGroup,
    internal: (u32, u32),
    _params: Vec<wgpu::Buffer>,
}

/// Draws one output.
pub struct Renderer {
    gpu: Arc<SceneGpu>,
    format: wgpu::TextureFormat,
    size: (u32, u32),
    scale: f32,
    uniform: wgpu::Buffer,
    g0: wgpu::BindGroup,
    scene_pipe: wgpu::RenderPipeline,
    post_pipe: wgpu::RenderPipeline,
    pre_pipe: wgpu::RenderPipeline,
    down_pipe: wgpu::RenderPipeline,
    up_pipe: wgpu::RenderPipeline,
    targets: Targets,
    scratch: Vec<f32>,
}

impl Renderer {
    /// `format` is the output view's format and must be sRGB (configure the
    /// surface with an sRGB view format). `size` in physical pixels, `scale`
    /// the internal resolution of the scene pass.
    pub fn new(
        device: &wgpu::Device,
        gpu: Arc<SceneGpu>,
        format: wgpu::TextureFormat,
        size: (u32, u32),
        scale: f32,
    ) -> Result<Renderer> {
        if !format.is_srgb() {
            return Err(anyhow!("the output format {format:?} isn't sRGB"));
        }
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ukiyo uniforms"),
            size: (gpu.layout.total_floats() * 4) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let g0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ukiyo g0"),
            layout: &gpu.g0,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&gpu.sampler_repeat),
                },
            ],
        });
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipe = |label: &str,
                    module: &wgpu::ShaderModule,
                    layouts: &[Option<&wgpu::BindGroupLayout>],
                    entry: &str,
                    fmt,
                    blend| {
            let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: layouts,
                immediate_size: 0,
            });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: fmt,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let add = Some(wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::One,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent::OVER,
        });
        let scene_pipe = pipe(
            "scene",
            &gpu.module,
            &[Some(&gpu.g0), Some(&gpu.g1)],
            "fs_scene",
            HDR,
            None,
        );
        let post_pipe = pipe(
            "post",
            &gpu.module,
            &[Some(&gpu.g0), Some(&gpu.g1), Some(&gpu.g2)],
            "fs_post",
            format,
            None,
        );
        let pre_pipe = pipe(
            "bloom pre",
            &gpu.bloom_module,
            &[Some(&gpu.bloom_bgl)],
            "fs_prefilter",
            HDR,
            None,
        );
        let down_pipe = pipe(
            "bloom down",
            &gpu.bloom_module,
            &[Some(&gpu.bloom_bgl)],
            "fs_down",
            HDR,
            None,
        );
        let up_pipe = pipe(
            "bloom up",
            &gpu.bloom_module,
            &[Some(&gpu.bloom_bgl)],
            "fs_up",
            HDR,
            add,
        );
        if let Some(e) = pollster_block(scope.pop()) {
            bail!("pipeline: {e}");
        }
        let targets = make_targets(device, &gpu, size, scale);
        Ok(Renderer {
            scratch: vec![0.0; gpu.layout.total_floats()],
            gpu,
            format,
            size,
            scale,
            uniform,
            g0,
            scene_pipe,
            post_pipe,
            pre_pipe,
            down_pipe,
            up_pipe,
            targets,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// New output size or internal scale (cheap when nothing changed).
    pub fn resize(&mut self, device: &wgpu::Device, size: (u32, u32), scale: f32) {
        if size == self.size && (scale - self.scale).abs() < 1e-3 {
            return;
        }
        self.size = size;
        self.scale = scale;
        self.targets = make_targets(device, &self.gpu, size, scale);
    }

    /// Record and submit one frame into `target`.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &Frame,
        target: &wgpu::TextureView,
    ) {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ukiyo frame"),
        });
        self.encode(queue, &mut enc, frame, target);
        queue.submit([enc.finish()]);
    }

    fn encode(
        &mut self,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        frame: &Frame,
        target: &wgpu::TextureView,
    ) {
        let (w, h) = self.size;
        let (cs, cc) = self.gpu.cover(w, h);
        self.scratch.clear();
        self.scratch.extend_from_slice(&frame.uniforms);
        self.scratch.resize(self.gpu.layout.total_floats(), 0.0);
        self.scratch[0..6].copy_from_slice(&[w as f32, h as f32, cs[0], cs[1], cc[0], cc[1]]);
        queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&self.scratch));

        let t = &self.targets;
        pass(
            enc,
            "scene",
            &t.hdr,
            &self.scene_pipe,
            &[&self.g0, &self.gpu.g1_group],
            false,
        );
        if !t.bloom.is_empty() {
            pass(
                enc,
                "bloom pre",
                &t.bloom[0].0,
                &self.pre_pipe,
                &[&t.pre],
                false,
            );
            for i in 0..t.bloom.len() - 1 {
                pass(
                    enc,
                    "bloom down",
                    &t.bloom[i + 1].0,
                    &self.down_pipe,
                    &[&t.down[i]],
                    false,
                );
            }
            for i in (0..t.bloom.len() - 1).rev() {
                pass(
                    enc,
                    "bloom up",
                    &t.bloom[i].0,
                    &self.up_pipe,
                    &[&t.up[i]],
                    true,
                );
            }
        }
        pass(
            enc,
            "post",
            target,
            &self.post_pipe,
            &[&self.g0, &self.gpu.g1_group, &t.g2],
            false,
        );
    }

    /// Render a frame into an offscreen texture and read it back as RGBA8
    /// (sRGB). For snapshots, the lock screen and recording.
    pub fn capture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        frame: &Frame,
    ) -> Result<image::RgbaImage> {
        let (w, h) = self.size;
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        // The post pipeline was built for self.format; rebuild for RGBA if needed.
        if self.format != wgpu::TextureFormat::Rgba8UnormSrgb {
            let mut r = Renderer::new(
                device,
                self.gpu.clone(),
                wgpu::TextureFormat::Rgba8UnormSrgb,
                self.size,
                self.scale,
            )?;
            return r.capture(device, queue, frame);
        }
        let view = tex.create_view(&Default::default());
        let row = (w * 4).div_ceil(256) * 256;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: u64::from(row) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        self.encode(queue, &mut enc, frame, &view);
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|e| anyhow!("waiting for the GPU: {e}"))?;
        rx.recv()
            .context("map callback")?
            .context("mapping the capture buffer")?;
        let mapped = slice
            .get_mapped_range()
            .map_err(|e| anyhow!("mapping: {e:?}"))?;
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            let start = y * row as usize;
            out.extend_from_slice(&mapped[start..start + (w * 4) as usize]);
        }
        drop(mapped);
        buf.unmap();
        image::RgbaImage::from_raw(w, h, out).context("capture size")
    }
}

fn pass(
    enc: &mut wgpu::CommandEncoder,
    label: &str,
    view: &wgpu::TextureView,
    pipe: &wgpu::RenderPipeline,
    groups: &[&wgpu::BindGroup],
    load: bool,
) {
    let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: if load {
                    wgpu::LoadOp::Load
                } else {
                    wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                },
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    rp.set_pipeline(pipe);
    for (i, g) in groups.iter().enumerate() {
        rp.set_bind_group(i as u32, Some(*g), &[]);
    }
    rp.draw(0..3, 0..1);
}

fn make_targets(device: &wgpu::Device, gpu: &SceneGpu, size: (u32, u32), scale: f32) -> Targets {
    let internal = (
        ((size.0 as f32 * scale).round() as u32).max(1),
        ((size.1 as f32 * scale).round() as u32).max(1),
    );
    let mk = |label: &str, (w, h): (u32, u32)| {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: HDR,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default())
    };
    let hdr = mk("hdr", internal);
    let mut bloom = Vec::new();
    if gpu.bloom.enabled {
        let (mut w, mut h) = internal;
        for _ in 0..gpu.bloom.levels {
            w = (w / 2).max(1);
            h = (h / 2).max(1);
            bloom.push((mk("bloom", (w, h)), (w, h)));
            if w == 1 && h == 1 {
                break;
            }
        }
    }
    let mut params = Vec::new();
    let mut group = |src: &wgpu::TextureView, src_size: (u32, u32)| {
        let data: [f32; 4] = [
            1.0 / src_size.0 as f32,
            1.0 / src_size.1 as f32,
            gpu.bloom.threshold,
            gpu.bloom.knee.max(1e-3),
        ];
        let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("bloom params"),
            contents: bytemuck::cast_slice(&data),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let g = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bloom"),
            layout: &gpu.bloom_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&gpu.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buf.as_entire_binding(),
                },
            ],
        });
        params.push(buf);
        g
    };
    let pre = group(&hdr, internal);
    let down: Vec<_> = (0..bloom.len().saturating_sub(1))
        .map(|i| group(&bloom[i].0, bloom[i].1))
        .collect();
    let up: Vec<_> = (0..bloom.len().saturating_sub(1))
        .map(|i| group(&bloom[i + 1].0, bloom[i + 1].1))
        .collect();
    let bloom_view = bloom.first().map_or(&gpu.black, |(v, _)| v);
    let g2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ukiyo g2"),
        layout: &gpu.g2,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&hdr),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(bloom_view),
            },
        ],
    });
    Targets {
        hdr,
        bloom,
        pre,
        down,
        up,
        g2,
        internal,
        _params: params,
    }
}

impl Renderer {
    /// The scene pass's size in pixels.
    pub fn internal_size(&self) -> (u32, u32) {
        self.targets.internal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_crops_the_long_side() {
        // 3:2 image on a 16:10 screen: full width, crop height.
        let (s, c) = cover((3000, 2000), (1600, 1000), [0.5, 0.5], 1.0);
        assert!((s[0] - 1.0).abs() < 1e-6 && (s[1] - 1.5 / 1.6).abs() < 1e-6);
        assert_eq!(c, [0.5, 0.5]);
        // Tall screen: full height, crop width.
        let (s, _) = cover((3000, 2000), (1000, 1000), [0.5, 0.5], 1.0);
        assert!((s[0] - 2.0 / 3.0).abs() < 1e-6 && s[1] == 1.0);
        // Focus is clamped so the crop stays inside the image.
        let (s, c) = cover((3000, 2000), (1600, 1000), [0.5, 0.0], 1.1);
        assert!((c[1] - s[1] / 2.0).abs() < 1e-6);
    }
}
