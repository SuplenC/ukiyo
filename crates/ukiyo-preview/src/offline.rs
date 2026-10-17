//! Headless rendering: single shots and scripted recordings. Time is
//! simulated at a fixed frame rate, so output is the same on every run.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use ukiyo_core::Scene;
use ukiyo_core::render::open_device;

use crate::host::Host;
use crate::hud::{Hud, Meter};
use crate::script::Script;
use crate::{Args, build_config, parse_signals};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

pub fn run(args: &Args) -> Result<()> {
    let scene = Scene::load(&args.scene)?;
    let config = build_config(args, &scene)?;
    let script = match &args.script {
        Some(p) => Some(
            Script::parse(&std::fs::read_to_string(p).with_context(|| p.display().to_string())?)
                .map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?,
        ),
        None => None,
    };
    let size = args
        .size
        .or(script.as_ref().and_then(|s| s.size.map(|[w, h]| (w, h))))
        .unwrap_or((1920, 1080));
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let (_adapter, device, queue) = pollster::block_on(open_device(&instance, None))?;
    let mut host = Host::load(
        &device,
        &queue,
        &args.scene,
        config,
        FORMAT,
        size,
        args.scale,
    )?;
    let signals = parse_signals(args)?;
    let hud = Hud::new();

    if let Some(frames) = args.bench {
        return bench(
            &device, &queue, &mut host, size, frames, args, &signals, &_adapter,
        );
    }
    match script {
        None => {
            // A shot: simulate up to `at`, render once.
            let fps = 30.0;
            let n = (args.at * fps).round() as u64;
            host.engine.set_clock_override(args.hour);
            host.engine.set_still(args.still);
            for (k, v) in &signals {
                host.engine.bus.set(k, *v);
            }
            let pulse_at = n.saturating_sub((args.pulse_before * fps).round() as u64);
            let mut frame = host.engine.tick(0.0);
            for i in 1..=n {
                if i == pulse_at {
                    for p in &args.pulses {
                        host.engine.bus.pulse(p);
                    }
                }
                frame = host.engine.tick(i as f64 / fps);
            }
            let img = host.renderer.capture(&device, &queue, &frame)?;
            let out = args.shot.as_ref().context("--shot")?;
            img.save(out).with_context(|| out.display().to_string())?;
            eprintln!("wrote {}", out.display());
        }
        Some(script) => {
            let fps = script.fps;
            let n = (script.duration * fps).round() as u64;
            let mut ff = match &args.record {
                Some(out) => Some(
                    Command::new("ffmpeg")
                        .args([
                            "-hide_banner",
                            "-loglevel",
                            "error",
                            "-y",
                            "-f",
                            "rawvideo",
                            "-pix_fmt",
                            "rgba",
                        ])
                        .args([
                            "-s",
                            &format!("{}x{}", size.0, size.1),
                            "-r",
                            &fps.to_string(),
                            "-i",
                            "-",
                        ])
                        .args([
                            "-c:v",
                            "libx264",
                            "-preset",
                            "slow",
                            "-crf",
                            "17",
                            "-pix_fmt",
                            "yuv420p",
                            "-movflags",
                            "+faststart",
                        ])
                        .arg(out)
                        .stdin(Stdio::piped())
                        .spawn()
                        .context("starting ffmpeg (is it installed?)")?,
                ),
                None => None,
            };
            if let Some(dir) = &args.frames {
                std::fs::create_dir_all(dir)?;
            }
            for (k, v) in &signals {
                host.engine.bus.set(k, *v);
            }
            let every = (args.every * fps).round().max(1.0) as u64;
            let mut prev_t = None;
            for i in 0..n {
                let t = i as f64 / fps;
                let st = script.state(t);
                for (k, v) in &st.values {
                    host.engine.bus.set(k, *v);
                }
                let (level, bass, mid, treble, beat) = match st.music {
                    Some((bpm, since)) => crate::script::fake_music(bpm, t - since),
                    None => (0.0, 0.0, 0.0, 0.0, 0.0),
                };
                for (k, v) in [
                    ("audio.level", level),
                    ("audio.bass", bass),
                    ("audio.mid", mid),
                    ("audio.treble", treble),
                    ("audio.beat", beat),
                ] {
                    host.engine.bus.set(k, v);
                }
                for p in script.pulses(prev_t, t) {
                    host.engine.bus.pulse(&p);
                }
                prev_t = Some(t);
                host.engine.set_clock_override(st.hour.or(args.hour));
                host.engine.set_still(st.still);
                let frame = host.engine.tick(t);
                let mut img = host.renderer.capture(&device, &queue, &frame)?;
                if args.hud {
                    draw_hud(&hud, &mut img, &host, &st);
                }
                if let Some(dir) = &args.frames
                    && i % every == 0
                {
                    img.save(dir.join(format!("f{i:05}-t{t:06.2}.png")))?;
                }
                if let Some(ff) = &mut ff {
                    ff.stdin
                        .as_mut()
                        .context("ffmpeg stdin")?
                        .write_all(img.as_raw())
                        .context("ffmpeg died")?;
                }
                if i % (fps as u64 * 5) == 0 {
                    eprintln!("{t:5.1} s / {}", script.duration);
                }
            }
            if let Some(mut ff) = ff {
                drop(ff.stdin.take());
                if !ff.wait()?.success() {
                    bail!("ffmpeg failed");
                }
                eprintln!(
                    "wrote {}",
                    args.record
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                );
            }
        }
    }
    Ok(())
}

fn draw_hud(hud: &Hud, img: &mut image::RgbaImage, host: &Host, st: &crate::script::State) {
    let bus = &host.engine.bus;
    let hour = host.engine.hour();
    let (h, m) = (
        hour.floor() as u32 % 24,
        ((hour.fract()) * 60.0).floor() as u32,
    );
    let state = if host.engine.still() >= 1.0 {
        "still (locked)".to_string()
    } else if host.engine.still() > 0.0 {
        "going still".to_string()
    } else {
        "running".to_string()
    };
    let fail = bus.get("pulse.build_failed");
    let pass = bus.get("pulse.build_passed");
    let build = if fail > 0.05 {
        "FAILED".to_string()
    } else if pass > 0.05 {
        "passed".to_string()
    } else {
        "-".to_string()
    };
    let meters = [
        Meter {
            label: "cursor",
            value: (bus.raw("cursor.x").abs() + bus.raw("cursor.y").abs()).min(1.0),
            text: None,
        },
        Meter {
            label: "typing",
            value: bus.raw("typing.rate"),
            text: None,
        },
        Meter {
            label: "cpu",
            value: bus.raw("cpu.load"),
            text: None,
        },
        Meter {
            label: "music",
            value: bus.raw("audio.level"),
            text: None,
        },
        Meter {
            label: "build",
            value: 0.0,
            text: Some(build),
        },
        Meter {
            label: "clock",
            value: 0.0,
            text: Some(format!("{h:02}:{m:02}")),
        },
        Meter {
            label: "state",
            value: 0.0,
            text: Some(state),
        },
    ];
    hud.meters(img, &meters);
    if let Some(c) = &st.caption {
        hud.caption(img, c, st.caption_age);
    }
}

/// Render `frames` frames into an offscreen target, waiting for the GPU after
/// each, and report the time per frame. On a software rasteriser this is
/// close to the CPU cost of a frame.
#[allow(clippy::too_many_arguments)]
fn bench(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    host: &mut Host,
    size: (u32, u32),
    frames: u32,
    args: &Args,
    signals: &[(String, f32)],
    adapter: &wgpu::Adapter,
) -> Result<()> {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bench"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    for (k, v) in signals {
        host.engine.bus.set(k, *v);
    }
    host.engine.set_clock_override(args.hour);
    let wait = || {
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map(|_| ())
    };
    // Warm up: pipelines, first uploads.
    for i in 0..3 {
        let f = host.engine.tick(f64::from(i) / 30.0);
        host.renderer.render(device, queue, &f, &view);
        wait().ok();
    }
    let t0 = std::time::Instant::now();
    // Wait every few frames: measures throughput, not submit latency.
    for i in 0..frames {
        let f = host.engine.tick(f64::from(i + 3) / 30.0);
        host.renderer.render(device, queue, &f, &view);
        if i % 4 == 3 {
            wait().ok();
        }
    }
    wait().ok();
    let per = t0.elapsed().as_secs_f64() * 1000.0 / f64::from(frames.max(1));
    let info = adapter.get_info();
    let internal = host.renderer.internal_size();
    println!(
        "{} ({:?}), {}x{}, scene pass {}x{}: {per:.2} ms per frame ({:.0} fps max)",
        info.name,
        info.backend,
        size.0,
        size.1,
        internal.0,
        internal.1,
        1000.0 / per
    );
    Ok(())
}
