# Writing a scene

A scene is a directory:

```
my-scene/
  scene.toml     what it is, its textures, its tunables, how signals drive it
  scene.wgsl     the shader: fs_scene, and optionally fs_post
  poster.jpg     a still, for pickers, colour extraction and lock screens
  *.jpg, *.png   textures
```

Put it in `~/.local/share/ukiyo/scenes/` (or anywhere on `UKIYO_SCENE_PATH`) and `ukiyo set my-scene`. While you work on it, run the preview: `ukiyo-preview my-scene/`. It reloads whenever a file in the directory changes, and shows shader errors in its panel instead of crashing.

`scenes/kabukicho-gate/` is the reference: read it next to this page.

## scene.toml

```toml
[scene]
name = "Kabukichō Gate"         # required
description = "…"
author = "…"
license = "CC-BY-SA-4.0"         # SPDX id of the assets
credits = ["Photo: …"]
poster = "poster.jpg"
shader = "scene.wgsl"            # default

[view]
frame = "base"                   # the texture whose aspect frames the scene (default: first)
focus = [0.5, 0.4]               # centre of the cover crop, image uv
overscan = 1.04                  # zoom so camera moves never show an edge (1..2)

[render]
fps = 30                         # cap while it moves; the user's cap applies too
scale = 1.0                      # internal resolution of the scene pass
animated = true                  # false: frames only when a binding changes (0 fps otherwise)
bloom = { threshold = 0.95, knee = 0.4, levels = 7 }   # or enabled = false

[textures]
base = { path = "base.jpg", color = "srgb" }   # photos: srgb, decoded to linear by the sampler
depth = "depth.png"                            # data: linear (the default)
```

A texture entry is a path, or a table with `path`, `color` (`srgb` or `linear`), `mipmaps` (default true) and `wrap` (`clamp` or `repeat`). 8-bit images load as RGBA8 (or R8 for greyscale), 16-bit greyscale (depth maps) as a 16-bit float texture. Mipmaps are built at load, so `textureSampleLevel(t, smp, uv, 6.0)` is a cheap blur.

### Params: what users can tune

```toml
[params.rain]
type = "float"                   # float | bool | choice | color
label = "Rain"
group = "Weather"
help = "How hard it rains when the machine is idle."
default = 0.45
min = 0.0
max = 1.0
step = 0.01                      # optional

[params.mood]
type = "choice"
label = "Mood"
options = ["calm", "storm"]      # the shader gets the index
default = "calm"

[params.tint]
type = "color"
label = "Tint"
default = "#ff1028"              # vec4<f32>, linear rgb, a = 1
```

Every param shows up in `ukiyo config schema` as `scenes.<id>.<name>`, so settings UIs get a widget for it without knowing your scene. Users override it in their config:

```toml
[scenes.kabukicho-gate]
rain = 0.8
```

### Bindings: signals to uniforms

```toml
[bindings]
flicker = { expr = "param.flicker_base + typing.rate * param.flicker_typing", attack = 0.05, release = 0.6 }
rain_amount = { expr = "clamp(param.rain + cpu.load * param.rain_cpu, 0, 1.2)", attack = 1.2, release = 3.0 }
cam_x = "cursor.x * param.parallax + workspace.pan * param.drift"
failed_age = "pulse.build_failed.age"
night = "clock.night"
```

A binding is an expression, or a table with `expr` and optional smoothing (`smooth` for both directions, or `attack` and `release`, in seconds: the time constant, so 63% of the way there after that long, 95% after three times) and a `min`/`max` clamp. Bindings are evaluated in file order, so a binding can read an earlier one by name. Signals arrive already scaled and smoothed per the user's config; a binding's smoothing is the scene's own shaping on top.

**Expressions:** numbers, names, `+ - * / % ^`, comparisons (`< <= > >= == !=`, giving 1 or 0), `&&` `||` `!`, `c ? a : b`, parentheses, `pi`, and `min max clamp saturate mix step smoothstep abs sign floor ceil fract sqrt pow exp log sin cos wave remap`. `wave(x)` is a 0..1 cosine with period 1; `remap(x, a, b, c, d)` maps [a, b] onto [c, d], clamped. Division by zero and other non-finite results read as 0.

**Names** an expression can read:

| Name | Range | What |
|---|---|---|
| `param.<name>` | | a param (a colour reads its red channel) |
| `<binding>` | | an earlier binding |
| `cursor.x`, `cursor.y` | -1..1 | the cursor on the output, 0 in the middle |
| `workspace.pan` | -1..1 | first workspace to ninth: for camera drift |
| `workspace.id` | | the active workspace id |
| `typing.rate` | 0..1 | about 8 keys a second is 1 |
| `cpu.load` | 0..1 | busy fraction of all CPUs |
| `audio.level`, `audio.bass`, `audio.mid`, `audio.treble` | 0..1 | what's playing (the output, never a microphone) |
| `audio.beat` | 0..1 | 1 on a detected beat, then falls with your smoothing |
| `clock.hour` | 0..24 | local time (or pinned by the user) |
| `clock.dusk` | 0..1 | 1 at the user's dusk hour, falling off over about two hours |
| `clock.night` | 0..1 | rises after dusk to 1 at the user's dead-of-night hour, gone by dawn |
| `clock.day` | 0..1 | 1 at midday |
| `pulse.<name>` | 0..gain | jumps to the pulse's gain when fired, decays (τ = 0.5 s) |
| `pulse.<name>.age` | s | seconds since it fired (a million before it ever did) |
| `state.still` | 0..1 | 0 running, 1 still (locked) |
| `state.battery` | 0/1 | on battery |

Pulses are events anyone can fire: `ukiyo pulse <name>`. The shell hook fires `build_passed` and `build_failed`. To get a pulse's strength without its decay (and shape the envelope from `.age` yourself), divide: `pulse * exp(age / 0.5)`.

Any other dotted name is a custom signal: `ukiyo signal weather.wind 0.7` sets it, and reads 0 until something does. A name that isn't dotted and isn't a param or binding is a typo; the engine warns about it and reads 0.

## scene.wgsl

ukiyo prepends a generated prelude, so your file starts with these in scope:

```wgsl
struct Ukiyo {
    resolution: vec2<f32>,   // the output, in pixels
    cover_scale: vec2<f32>,  // used by image_uv()
    cover_center: vec2<f32>,
    time: f32,               // scene seconds: slows to a stop when the scene goes still
    dt: f32,
    still: f32,              // 0 running .. 1 still
    frame: f32,
    seed: f32,               // per scene, 0..1
    wall: f32,               // seconds since the engine started
    // then every colour param (vec4<f32>), then every other param and binding (f32), by name
}
@group(0) @binding(0) var<uniform> u: Ukiyo;
@group(0) @binding(1) var smp: sampler;          // linear, clamp, mipmapped
@group(0) @binding(2) var smp_repeat: sampler;   // linear, repeat
@group(1) @binding(N) var t_<name>: texture_2d<f32>;   // every [textures] entry, alphabetical
@group(2) @binding(0) var t_scene: texture_2d<f32>;    // fs_post only: the scene pass, HDR
@group(2) @binding(1) var t_bloom: texture_2d<f32>;    // fs_post only: the bloom
struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> }  // uv: 0,0 top left
fn image_uv(screen: vec2<f32>) -> vec2<f32>   // screen uv to frame-texture uv (cover crop)
fn aces(x: vec3<f32>) -> vec3<f32>
```

You write:

- `@fragment fn fs_scene(in: VsOut) -> @location(0) vec4<f32>`: the scene, in linear light, HDR. Values above the bloom threshold glow. It runs at `render.scale` times the output resolution (times the user's `performance.scale`).
- `@fragment fn fs_post(in: VsOut) -> @location(0) vec4<f32>` (optional): runs at full resolution into the sRGB output, reading `t_scene` and `t_bloom`. Tonemap, grade, vignette, grain, lens effects. Without one you get `aces(scene + 0.5 * bloom)`.

Keep it cheap. A wallpaper runs all day: the gate scene is 30 fps, and when the scene is still or paused nothing runs at all. Branch on masks (`if (mask > 0.0) { … }`) around expensive work, use mip levels instead of blur loops, and test with the preview's frame counter at `--scale 0.5` too.

## Assets: depth and masks

The gate uses a depth map (`depth.png`, 16-bit, 1 = nearest) for parallax and to put rain in front of and behind things, and masks (`emissive.png`, `regions.png`) for what glows, what flickers and what's sky. They're made offline, once, by `tools/` (Depth Anything V2 Small for depth, thresholds and polygons in `masks.toml` for the masks), so the result is reproducible from the source photo. See `tools/README.md`.

Licences: everything in a scene directory must allow redistribution. Say what each file is and where it came from in `scene.toml`'s `credits` and in `CREDITS.md`.
