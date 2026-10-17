# Configuration

Everything about the wallpaper is configurable, and nothing needs a restart: the daemon watches its config files and applies changes live.

## Files

All optional. Later ones win:

1. `$XDG_CONFIG_DIRS/ukiyo/config.toml` (usually `/etc/xdg/ukiyo/config.toml`): system-wide.
2. `$XDG_CONFIG_HOME/ukiyo/defaults.toml` (`~/.config/ukiyo/defaults.toml`): declarative defaults. Meant for home-manager, a dotfiles repo or a distribution; it can be a read-only link.
3. `$XDG_CONFIG_HOME/ukiyo/config.toml`: yours. `ukiyo config set` writes here (keeping your comments), and so does a settings UI.

Any option can also go under `[scenes.<id>]`, where it applies to that scene only. Within a file, the scene's value beats the global one; across files, a later file beats an earlier one, so your global `intensity` beats a scene override in `defaults.toml`. A value that doesn't fit (wrong type, unknown choice) is skipped with a warning, and the next layer down is used; numbers out of range are clamped.

```toml
# ~/.config/ukiyo/config.toml
scene = "kabukicho-gate"
intensity = 0.8

[performance]
on_battery = "still"

[signals.typing]
release = 3.0          # the neon takes longer to settle after you stop

[signals.build]
commands = ["cargo", "just", "make", "bacon"]

[scenes.kabukicho-gate]
rain = 0.7
fail_color = "#ff3050"
performance.fps = 24   # a global option, for this scene only
```

## From the command line

```sh
ukiyo config get intensity
ukiyo config set intensity 0.6                      # applies now and persists
ukiyo config set scenes.kabukicho-gate.rain 0.9
ukiyo config set signals.build.commands cargo,just,make
ukiyo config reset intensity                        # back to the default
ukiyo config reset scenes.kabukicho-gate            # all of a scene's overrides
ukiyo config schema --json                          # every option, for UIs
```

`schema --json` lists every option of the running scene (or `--scene <id>`), one object each:

```json
{"key": "scenes.kabukicho-gate.rain", "type": "float", "min": 0.0, "max": 1.0, "step": 0.01,
 "label": "Rain", "help": "How hard it rains when the machine is idle.", "group": "Kabukichō Gate: Weather",
 "default": 0.45, "value": 0.7, "source": "user"}
```

`type` is `float` (with `min`, `max`, `step` and maybe `unit`), `bool`, `choice` (with `options`), `color` (`"#rrggbb"`), `string` or `list`. `source` says which file set the value: `user`, `defaults`, `system`, or `null` for the default. That's enough to generate a settings panel without knowing a single option by name; `ukiyo-preview` and KubaOS's Quickshell panel both do.

## Every option

The global options are below. Each scene adds its own under `scenes.<id>.*`; the gate's are listed after them. These tables are generated from the schema (`cargo run -p ukiyo-core --example config_doc -- scenes/kabukicho-gate`), and a test fails if one goes missing here.

Signals have the same four knobs each: `enabled` (off means the source doesn't run at all), `gain` (multiplies the raw value, before the global `intensity`), and `attack` and `release`, smoothing time constants in seconds while the value rises and falls.

### General

| Key | Type | Default | What |
|---|---|---|---|
| `scene` | string | `""` | Scene. Id of the scene to show. Empty: none. |
| `intensity` | 0 to 2 × | `1.0` | Reactivity. Master gain on every reaction (cursor, typing, CPU, music, pulses). 0 is a calm, still-reacting-to-the-clock wallpaper. |

### Performance

| Key | Type | Default | What |
|---|---|---|---|
| `performance.motion` | bool | `true` | Animate. Off: draw one frame and hold it. |
| `performance.fps` | 5 to 144 fps | `60.0` | Frame rate cap. Scenes ask for their own rate; this caps it. |
| `performance.scale` | 0.25 to 1 × | `1.0` | Render scale. Multiplies the scene's internal resolution. Lower is softer and cheaper. |
| `performance.on_battery` | full \| reduced \| still | `reduced` | On battery. full: as on AC. reduced: half the frame rate, 0.75 scale. still: hold a frame. |
| `performance.pause_on_fullscreen` | bool | `true` | Pause under fullscreen. Stop drawing while a fullscreen window covers the output. |
| `performance.idle_timeout` | 0 to 3600 s | `600.0` | Pause when idle. Seconds without input before drawing stops. 0: never. |

### Time of day

| Key | Type | Default | What |
|---|---|---|---|
| `clock.mode` | follow \| pinned | `follow` | Time of day. follow: the real clock. pinned: always `clock.hour`. |
| `clock.hour` | 0 to 24 h | `22.0` | Pinned hour. Used when the time of day is pinned. |
| `clock.dusk` | 14 to 22 h | `18.0` | Dusk. When dusk peaks. |
| `clock.night` | 0 to 6 h | `3.0` | Dead of night. When the night is deepest. |

### Lock screen

| Key | Type | Default | What |
|---|---|---|---|
| `lock.snapshot` | bool | `true` | Lock screen frame. When going still, save the frame for the lock screen (`ukiyo status` has the paths). |
| `lock.transition` | 0 to 5 s | `1.5` | Time to go still. How long the scene takes to settle when told to go still. |

### Cursor parallax

| Key | Type | Default | What |
|---|---|---|---|
| `signals.cursor.enabled` | bool | `true` | On |
| `signals.cursor.gain` | 0 to 3 × | `1.0` | Strength |
| `signals.cursor.attack` | 0 to 5 s | `0.25` | Attack. Smoothing while the signal rises. |
| `signals.cursor.release` | 0 to 10 s | `0.25` | Release. Smoothing while it falls. |
| `signals.cursor.source` | auto \| hyprland \| pointer | `auto` | Source. hyprland: the compositor's cursor everywhere. pointer: only over the bare desktop. auto: hyprland if it's there. |

### Workspace drift

| Key | Type | Default | What |
|---|---|---|---|
| `signals.workspace.enabled` | bool | `true` | On |
| `signals.workspace.gain` | 0 to 3 × | `1.0` | Strength |
| `signals.workspace.attack` | 0 to 5 s | `0.699999988079071` | Attack. Smoothing while the signal rises. |
| `signals.workspace.release` | 0 to 10 s | `0.699999988079071` | Release. Smoothing while it falls. |

### Typing

| Key | Type | Default | What |
|---|---|---|---|
| `signals.typing.enabled` | bool | `true` | On |
| `signals.typing.gain` | 0 to 3 × | `1.0` | Strength |
| `signals.typing.attack` | 0 to 5 s | `0.15000000596046448` | Attack. Smoothing while the signal rises. |
| `signals.typing.release` | 0 to 10 s | `1.5` | Release. Smoothing while it falls. |
| `signals.typing.source` | auto \| idle \| helper | `auto` | Source. idle: unprivileged, from the compositor's idle notifications. helper: exact rate from ukiyo-activityd. auto: helper if its socket answers. |
| `signals.typing.helper_socket` | string | `/run/ukiyo-activity/socket` | Helper socket. Where ukiyo-activityd listens. |

### CPU load

| Key | Type | Default | What |
|---|---|---|---|
| `signals.cpu.enabled` | bool | `true` | On |
| `signals.cpu.gain` | 0 to 3 × | `1.0` | Strength |
| `signals.cpu.attack` | 0 to 5 s | `1.0` | Attack. Smoothing while the signal rises. |
| `signals.cpu.release` | 0 to 10 s | `3.0` | Release. Smoothing while it falls. |
| `signals.cpu.window` | 0.25 to 5 s | `1.0` | Averaging window |

### Music

| Key | Type | Default | What |
|---|---|---|---|
| `signals.audio.enabled` | bool | `true` | On |
| `signals.audio.gain` | 0 to 3 × | `1.0` | Strength |
| `signals.audio.attack` | 0 to 5 s | `0.029999999329447746` | Attack. Smoothing while the signal rises. |
| `signals.audio.release` | 0 to 10 s | `0.18000000715255737` | Release. Smoothing while it falls. |
| `signals.audio.device` | string | `""` | Sink. PipeWire sink whose monitor is analysed. Empty: the default sink. Never a microphone. |

### Pulses (build results)

| Key | Type | Default | What |
|---|---|---|---|
| `signals.pulse.enabled` | bool | `true` | On |
| `signals.pulse.gain` | 0 to 3 × | `1.0` | Strength |
| `signals.build.commands` | list of strings | `cargo, just, make, ninja, cmake, meson, npm, pnpm, go, gradle, mvn, nix` | Build commands. A finished command whose first word is one of these sends build-passed or build-failed. |
| `signals.build.min_duration` | 0 to 600 s | `10.0` | Any command longer than. Other commands count as builds when they ran at least this long. 0: never. |

### Kabukichō Gate: Weather

| Key | Type | Default | What |
|---|---|---|---|
| `scenes.kabukicho-gate.rain` | 0 to 1 | `0.45` | Rain. How hard it rains when the machine is idle. |
| `scenes.kabukicho-gate.rain_cpu` | 0 to 1 | `0.55` | Rain from CPU load. How much heavier the rain gets while the CPU works. |
| `scenes.kabukicho-gate.wind` | -0.6 to 0.6 | `0.18` | Wind |
| `scenes.kabukicho-gate.haze` | 0 to 1 | `0.5` | Haze. Wet air down the street, lit by the signs. |
| `scenes.kabukicho-gate.lens` | 0 to 1 | `0.3` | Drops on the lens |

### Kabukichō Gate: Neon

| Key | Type | Default | What |
|---|---|---|---|
| `scenes.kabukicho-gate.flicker_base` | 0 to 1 | `0.08` | Flicker at rest |
| `scenes.kabukicho-gate.flicker_typing` | 0 to 2 | `1.0` | Flicker from typing |
| `scenes.kabukicho-gate.chase` | 0 to 1 | `0.3` | Gate bulb chase |
| `scenes.kabukicho-gate.music` | 0 to 2 | `1.0` | Billboard pulse. How hard the billboards pulse to music. |
| `scenes.kabukicho-gate.closing` | 0 to 1 | `0.4` | Signs that close at night. Share of the signs that go dark towards the dead of night. |

### Kabukichō Gate: Builds

| Key | Type | Default | What |
|---|---|---|---|
| `scenes.kabukicho-gate.fail_color` | colour | `#ff1028` | Build failed |
| `scenes.kabukicho-gate.pass_color` | colour | `#18ff7a` | Build passed |

### Kabukichō Gate: Camera

| Key | Type | Default | What |
|---|---|---|---|
| `scenes.kabukicho-gate.parallax` | 0 to 2 | `1.0` | Parallax depth |
| `scenes.kabukicho-gate.drift` | 0 to 2 | `1.0` | Workspace drift |

### Kabukichō Gate: Look

| Key | Type | Default | What |
|---|---|---|---|
| `scenes.kabukicho-gate.exposure` | 0.4 to 1.6 | `1.0` | Exposure |
| `scenes.kabukicho-gate.saturation` | 0 to 1.5 | `1.0` | Saturation |
| `scenes.kabukicho-gate.bloom` | 0 to 2 | `1.0` | Bloom |
| `scenes.kabukicho-gate.grain` | 0 to 1 | `0.35` | Film grain |
| `scenes.kabukicho-gate.vignette` | 0 to 1 | `0.5` | Vignette |

