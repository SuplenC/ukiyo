# Design

ukiyo draws a wallpaper that reacts to what you're doing: where the cursor is, how fast you type, how busy the CPU is, whether the last build passed, what's playing, what time it is, whether the screen is locked. This page is how it's put together and why.

## Pieces

```
crates/
  ukiyo-core       platform-neutral: scene format, expressions, signals, config, engine, wgpu renderer, IPC types
  ukiyo            the Wayland daemon and the CLI (one binary)
  ukiyo-activityd  optional privileged helper: an aggregate typing rate from evdev, nothing else
  ukiyo-preview    winit + egui desktop app (macOS too): fake every signal, tune every option, record videos
scenes/            scenes shipped with the engine (kabukicho-gate)
tools/             offline asset pipeline: source download, depth maps, masks
demos/             scripted signal timelines for the preview's recorder
```

`ukiyo-core` knows nothing about Wayland, Hyprland or Linux. Everything that touches the platform (the layer surface, the compositor's sockets, `/proc`, PipeWire, evdev) lives in `ukiyo` and `ukiyo-activityd`. That's what lets a scene be built and tuned on a Mac with Metal and run unchanged on a laptop with Vulkan, or in a VM on llvmpipe.

## The frame

```
sources ──raw values──▶ SignalBus ──gain, intensity, attack/release (user config)──▶ values
                                                                                        │
scene.toml [bindings]: expressions + the scene's own smoothing ◀────────────────────────┘
        │
        ▼
Engine::tick(now) ──▶ Frame (one uniform block: built-ins, params, bindings)
        │
        ▼
Renderer: scene pass (fs_scene, HDR, internal resolution) → bloom chain → post pass (fs_post, sRGB output)
```

**Signals** are named floats (`cpu.load`, `typing.rate`, `pulse.build_failed`). Sources write raw values; the bus applies the user's per-group settings (on/off, gain, attack, release) and the global intensity once per frame, frame-rate independently. A signal nobody writes reads 0, so every source is optional and a scene never breaks because a machine has no PipeWire or isn't running Hyprland. Pulses are events: `pulse.<name>` jumps and decays, `pulse.<name>.age` counts seconds since it fired, so a shader can draw its own envelope.

**Bindings** turn signals into uniforms with a small expression language (`typing.rate * 0.8 + cpu.load * 0.3`), compiled once to a tree over slots, evaluated per frame without allocating, and smoothed. Scene authors never write Rust. **Params** are the scene's tunables, declared with type, range, default and label, and exposed through the config schema.

**The engine** owns the scene clock. Going still (the lock) doesn't cut to a frozen frame: the clock slows with a smoothstep over `lock.transition` seconds until it stops, so rain decelerates and the neon settles, and then the engine reports it wants no more frames.

**The renderer** is three passes. The scene pass runs the author's `fs_scene` at `render.scale × performance.scale` of the output into an RGBA16F target, so emissive things can exceed 1 and bloom. Bloom is built in: a soft-knee bright pass at half resolution, a 13-tap downsample chain and a tent-filter upsample (the usual Call of Duty: AW filter). The post pass runs `fs_post` at full resolution into the sRGB output, where tonemapping and lens effects live. Textures and the compiled shader are shared by every output on a device (`SceneGpu`); each output has its own targets and uniforms (`Renderer`). Shader errors come back as errors with naga's message (the preview shows them and keeps the last good scene).

## Scenes

A scene is a photo made to move, the way Wallpaper Engine's best "scene" wallpapers are, rather than a fully procedural shader: a base photo, a depth map, masks for what emits light, and a shader that layers effects on top. The photo carries the realism; the shader carries the life. Depth and masks are generated offline once (Depth Anything V2 Small, then thresholds and polygons kept in `masks.toml`) and committed as assets, so the runtime does no ML and the result is reproducible from the source photo. See `SCENE-FORMAT.md`.

The gate scene, layer by layer: parallax by depth (a few fixed-point iterations on a blurred depth, so edges don't tear); emissive boost and per-sign flicker (each lit sign has a random id in the mask's alpha, so signs stutter independently, in quarter-second bursts at 30 Hz, like failing neon); the gate's bulbs (a slow marquee chase, the odd sagging filament, and the red strobe or green sweep for builds, with light spilling onto the street); a time-of-day grade (twilight sky at dusk, colder and darker with signs switching off towards 3 am); haze and drifting mist that depth fades into; six sheets of rain at different depths, each drop shown only where the scene behind it is farther away, so rain falls both behind and in front of the gate; drops bursting on the top of the arch; and in post, drops on the lens, chromatic aberration, a warm bloom halation, a highlight shoulder, vignette and grain.

## The daemon

One process per Wayland session. It puts a `zwlr_layer_shell_v1` surface on the **background** layer of every output (namespace `ukiyo`, all edges, exclusive zone -1, no keyboard) through smithay-client-toolkit, and renders into it with wgpu (Vulkan, or GL where that's all there is). HiDPI: `wp_fractional_scale_v1` with `wp_viewporter` where the compositor has them (buffer = logical size × scale, so a 1440×900 logical panel at scale 2 is drawn at exactly 2880×1800), integer buffer scale otherwise.

**Frame pacing follows the compositor.** A frame is drawn only when the compositor's frame callback for the previous one has arrived and the scene's frame interval has passed, so an output the compositor isn't showing costs nothing. When nothing is moving (the scene is still, paused, or a non-animated scene's bindings have settled), the daemon stops requesting frames: 0 fps, no wakeups except its signal sources, which also stop when they have nobody to feed (audio capture, cursor polling).

**Pausing** is a set of reasons, and drawing stops while any is present: `pause` from the CLI (with a reason, so a desktop shell and a user don't fight over it), a fullscreen window on that output, no input for `performance.idle_timeout` (ext-idle-notify-v1). The battery rule is the engine's (`performance.on_battery`: full, reduced, still).

**IPC** is a Unix socket per Wayland display (`$XDG_RUNTIME_DIR/ukiyo/<display>.sock`, in a 0700 directory), one JSON object per line each way, so a shell can talk to it with `socat` or zsh's `zsocket`. The CLI is a thin client. `config set` validates against the schema, persists to the user's file (keeping comments) and applies at once; edits to the files by hand are picked up by a watcher.

## Signal sources

| Signal | Source | Notes |
|---|---|---|
| cursor | Hyprland's `j/cursorpos`, polled at most once per drawn frame; else `wl_pointer` over our own surface | relative to the output under the cursor |
| workspace | Hyprland's event socket (`.socket2.sock`) | `workspace.pan`: workspace 1 to 9 maps to -1..1, the scene smooths it into a camera drift; also the fullscreen pause |
| typing | ext-idle-notify-v1 with a short timeout (default), or `ukiyo-activityd` | see below |
| cpu | `/proc/stat` deltas over `signals.cpu.window` | |
| audio | PipeWire: the default sink's **monitor**, via `pw-record` (or `parec`), FFT in-process | only while drawing; never a microphone |
| builds | `ukiyo command` / the shell hook | the daemon decides what's a build (`signals.build.*`) |
| clock | the local time | `clock.dusk` and `clock.night` from the user's hours |
| lock, idle, fullscreen, AC | `ukiyo still / pause / resume`, the compositor | a desktop shell can drive these with its own rules |

### Keystrokes and privacy

Wayland deliberately gives clients no global key events, and the way around it people reach for, adding the user to the `input` group, lets every process the user runs read every keyboard: a keylogger for free. ukiyo doesn't do that. There are two typing sources:

- **`idle` (the default, no privileges at all).** The compositor's idle-notify protocol with a timeout of a few hundred milliseconds says when the user starts and stops interacting. Activity while the cursor isn't moving is, near enough, typing. It can't tell 3 keys a second from 10, but "flicker while I type, settle when I stop" doesn't need to.
- **`helper` (opt-in, exact rate).** `ukiyo-activityd` is a tiny system service, the only thing in the `input` group, heavily sandboxed by systemd. It counts key presses (never which key), and publishes only a rate over a sliding window, quantised and updated at a coarse fixed cadence, so inter-keystroke timing, a known side channel for recovering typed text and passwords, never leaves it. Only the user of the active seat can connect, and it publishes 0 while their session isn't in front.

`SECURITY.md` has the threat model.

## The lock screen

A session-lock surface covers the background layer, so whatever the wallpaper draws under the lock is invisible. ukiyo's job is to stop wasting power and to hand the locker a picture: on `still`, it writes the frame that's on screen right now to `$XDG_RUNTIME_DIR/ukiyo/still-<output>.png`, then eases to a standstill and stops drawing. A locker shows that file (blurred, typically), so the lock fades in over exactly what was there, at zero GPU cost while locked. Running a second live renderer inside the locker would mean reimplementing every scene in another shader language and burning battery behind a password prompt.

## Configuration

Layered TOML (system, declarative defaults, user), per-scene overrides at `[scenes.<id>]`, a self-describing schema (`ukiyo config schema --json`) that settings UIs are generated from, and hot reload. `CONFIG.md` lists every option.

## Packaging

A Nix flake (`flake.nix`) builds the daemon, the helper and the scenes into one package, with the GPU libraries on the RPATH so it runs on non-NixOS too. Scenes are found on `$XDG_DATA_DIRS/ukiyo/scenes`, so a package manager drops them in `/usr/share/ukiyo/scenes` and a user drops their own in `~/.local/share/ukiyo/scenes`.

## Decisions that could have gone the other way

- **wgpu, not raw GL or Vulkan:** one renderer for Vulkan on Linux, Metal on the Mac preview, and GL on software-rendered VMs, with WGSL validated by naga before it runs.
- **Photo scenes, not procedural ones:** a real photo of Kabukichō will always beat a shader's idea of it. Procedural work is for what moves.
- **Offline depth, not runtime:** a depth model at runtime would cost more than the whole renderer, for a map that never changes.
- **Expressions in TOML, not scripting:** scenes stay data, safe to download and share; no Lua, no WASM, no plugin ABI to keep stable.
- **One process, many outputs:** shared textures and one signal bus; per-output frame pacing.
- **JSON lines over a socket, not D-Bus:** trivially scriptable, no bus needed, the same from a shell hook and a desktop shell.
