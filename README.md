# ukiyo 浮世

**Live wallpapers that react to what you're doing.** For Wayland.


ukiyo draws a scene behind your windows, a photo made to move, and wires it to your desktop. Move the cursor and the street shifts in depth. Type and the neon stutters, then settles when you stop. Run `cargo build` and the rain gets heavy. If the build fails, the gate strobes red; when it passes, green sweeps across it. Play music and the billboards pulse. At 18:00 the sky turns blue, at 3 am half the shops close. Lock the screen and everything goes still, and stops costing anything.

Think Wallpaper Engine's scene wallpapers, driven by real signals instead of a mouse-over effect, and built for a laptop battery: frames follow the compositor's frame callbacks, and when nothing moves ukiyo draws nothing at all.

A demo video and screenshots are being retaken; `just demo` renders the scripted demo yourself.

## What it reacts to

| Signal | From | Scene gets |
|---|---|---|
| Cursor | Hyprland's cursor position, or the pointer over the desktop | `cursor.x`, `cursor.y` |
| Workspace | Hyprland's event socket | `workspace.pan`, `workspace.id` |
| Typing | the compositor's idle notifications (no privileges), or an optional sandboxed helper | `typing.rate` |
| CPU | `/proc/stat` | `cpu.load` |
| Music | the PipeWire output monitor (never a microphone) | `audio.level`, `bass`, `mid`, `treble`, `beat` |
| Builds | a shell hook (`ukiyo shell-hook zsh`) | `pulse.build_failed`, `pulse.build_passed` |
| Anything | `ukiyo signal <name> <value>`, `ukiyo pulse <name>` | your own names |
| Time | the clock | `clock.hour`, `clock.dusk`, `clock.night` |
| Lock, idle, fullscreen, battery | the compositor, or your desktop shell via `ukiyo still / pause / resume` | stillness, 0 fps |

Every source is optional. On a compositor without Hyprland's sockets you lose the global cursor and the workspace drift; nothing breaks.

## Install

ukiyo needs a compositor with `wlr-layer-shell` (Hyprland, Sway, river, niri, Wayfire, KDE Plasma, …) and a GPU driver with Vulkan or OpenGL (software rendering works, slowly).

With Nix:

```sh
nix run github:SuplenC/ukiyo -- daemon        # or: nix profile install github:SuplenC/ukiyo
```

A Home Manager module is in `nix/hm-module.nix`.

With Cargo (Rust 1.88+), from git (the crates.io names are still pending):

```sh
cargo install --git https://github.com/SuplenC/ukiyo ukiyo
git clone https://github.com/SuplenC/ukiyo && mkdir -p ~/.local/share/ukiyo/scenes && cp -r ukiyo/scenes/* ~/.local/share/ukiyo/scenes/
ukiyo daemon &                       # or start it from your compositor's autostart
```

Then:

```sh
ukiyo scenes                         # what's installed
ukiyo set kabukicho-gate
ukiyo status
echo 'eval "$(ukiyo shell-hook zsh)"' >> ~/.zshrc    # builds pulse the scene
```

The shell hook doesn't spawn a process per prompt: it writes one line to the daemon's socket with zsh's `zsocket`, and the daemon decides what counts as a build (`cargo`, `just`, `make` and friends, or anything that ran longer than 10 s; both configurable).

To have a desktop shell or locker drive it:

```sh
ukiyo still        # on lock: save the current frame for the locker, ease to a standstill, stop drawing
ukiyo resume       # on unlock
ukiyo pause --reason mine    # stop drawing now; resume --reason mine undoes only yours
```

On `still`, the frame on screen is written to `$XDG_RUNTIME_DIR/ukiyo/still-<output>.png`, so a lock screen can show exactly what was there.

## Configure

Everything is configurable, live, from `~/.config/ukiyo/config.toml` or the CLI: every signal's on/off, strength and smoothing and its source options, the global reactivity, frame rate cap, render scale, battery behaviour, idle timeout, the time of day (follow the clock, or pin it, with your own dusk and night hours), and every knob a scene exposes (rain, wind, flicker, bloom, colours, …).

```sh
ukiyo config set intensity 0.6
ukiyo config set scenes.kabukicho-gate.rain 0.9
ukiyo config schema --json          # every option with type, range, default, current value and label
```

The schema is what settings UIs are generated from. [docs/CONFIG.md](docs/CONFIG.md) lists every option.

## Make scenes

A scene is a directory: `scene.toml`, a WGSL shader and textures. Signals reach the shader through a small expression language in the TOML, so scene authors never touch Rust:

```toml
[bindings]
flicker = { expr = "param.flicker_base + typing.rate * param.flicker_typing", attack = 0.05, release = 0.6 }
rain_amount = { expr = "clamp(param.rain + cpu.load * param.rain_cpu, 0, 1.2)", attack = 1.2, release = 3.0 }
```

Build and tune it on any desktop, macOS included, with the preview app: every signal on a slider or a key, every option in a generated panel, live reload on save, and a recorder for scripted timelines.

```sh
cargo run --release -p ukiyo-preview -- scenes/kabukicho-gate
cargo run --release -p ukiyo-preview -- scenes/kabukicho-gate --script demos/kabukicho-gate.toml --record out/demo.mp4 --hud
```

A [`justfile`](justfile) wraps the common tasks: `just` lists them (`test`, `preview`, `bench`, `demo`, `fmt`, `lint`).

[docs/SCENE-FORMAT.md](docs/SCENE-FORMAT.md) is the author's guide; `tools/` makes depth maps and masks from a photo.

## How it works

[docs/DESIGN.md](docs/DESIGN.md): the engine, the renderer, frame pacing, the signal model, the lock screen. [docs/SECURITY.md](docs/SECURITY.md): why ukiyo never needs to read your keyboard, and what the optional helper can and can't see. [docs/SCENES.md](docs/SCENES.md): the scenes planned next.

## Privacy

- The typing signal is a rate only. By default it comes from the compositor's idle notifications; the optional `ukiyo-activityd` helper is sandboxed and exposes one quantised number, never key codes or text.
- Audio comes from the PipeWire sink monitor (what you play), never from a microphone.
- Details in [docs/SECURITY.md](docs/SECURITY.md).

## Status

0.1. One scene. KubaOS is the first consumer; ukiyo needs nothing from it. Linux/Wayland for the daemon; the preview runs on Linux, macOS and Windows. Tested on Hyprland.

## Licence

Code: MIT OR Apache-2.0, at your option. Scenes carry their own asset licences: the Kabukichō gate photo is Basile Morin's, CC BY-SA 4.0, and so are the depth map, masks and poster derived from it. See [CREDITS.md](CREDITS.md).
