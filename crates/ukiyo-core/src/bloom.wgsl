// ukiyo's built-in bloom: a soft-knee bright pass into half resolution,
// a 13-tap downsample chain, then a 9-tap tent upsample added back up the
// chain (the Call of Duty: Advanced Warfare filter, as in most engines).

struct Params {
    texel: vec2<f32>,
    threshold: f32,
    knee: f32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var<uniform> p: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
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

fn down13(uv: vec2<f32>) -> vec3<f32> {
    let t = p.texel;
    let a = textureSampleLevel(src, smp, uv + t * vec2(-2.0, -2.0), 0.0).rgb;
    let b = textureSampleLevel(src, smp, uv + t * vec2(0.0, -2.0), 0.0).rgb;
    let c = textureSampleLevel(src, smp, uv + t * vec2(2.0, -2.0), 0.0).rgb;
    let d = textureSampleLevel(src, smp, uv + t * vec2(-2.0, 0.0), 0.0).rgb;
    let e = textureSampleLevel(src, smp, uv, 0.0).rgb;
    let f = textureSampleLevel(src, smp, uv + t * vec2(2.0, 0.0), 0.0).rgb;
    let g = textureSampleLevel(src, smp, uv + t * vec2(-2.0, 2.0), 0.0).rgb;
    let h = textureSampleLevel(src, smp, uv + t * vec2(0.0, 2.0), 0.0).rgb;
    let i = textureSampleLevel(src, smp, uv + t * vec2(2.0, 2.0), 0.0).rgb;
    let j = textureSampleLevel(src, smp, uv + t * vec2(-1.0, -1.0), 0.0).rgb;
    let k = textureSampleLevel(src, smp, uv + t * vec2(1.0, -1.0), 0.0).rgb;
    let l = textureSampleLevel(src, smp, uv + t * vec2(-1.0, 1.0), 0.0).rgb;
    let m = textureSampleLevel(src, smp, uv + t * vec2(1.0, 1.0), 0.0).rgb;
    return e * 0.125 + (a + c + g + i) * 0.03125 + (b + d + f + h) * 0.0625 + (j + k + l + m) * 0.125;
}

@fragment
fn fs_prefilter(in: VsOut) -> @location(0) vec4<f32> {
    // Clamp fireflies before they smear into the whole chain.
    let c = min(down13(in.uv), vec3(64.0));
    let br = max(c.r, max(c.g, c.b));
    let soft = clamp(br - p.threshold + p.knee, 0.0, 2.0 * p.knee);
    let soft2 = soft * soft / (4.0 * p.knee + 1e-4);
    let w = max(soft2, br - p.threshold) / max(br, 1e-4);
    return vec4<f32>(c * w, 1.0);
}

@fragment
fn fs_down(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(down13(in.uv), 1.0);
}

@fragment
fn fs_up(in: VsOut) -> @location(0) vec4<f32> {
    let t = p.texel;
    var s = textureSampleLevel(src, smp, in.uv, 0.0).rgb * 4.0;
    s += textureSampleLevel(src, smp, in.uv + t * vec2(-1.0, 0.0), 0.0).rgb * 2.0;
    s += textureSampleLevel(src, smp, in.uv + t * vec2(1.0, 0.0), 0.0).rgb * 2.0;
    s += textureSampleLevel(src, smp, in.uv + t * vec2(0.0, -1.0), 0.0).rgb * 2.0;
    s += textureSampleLevel(src, smp, in.uv + t * vec2(0.0, 1.0), 0.0).rgb * 2.0;
    s += textureSampleLevel(src, smp, in.uv + t * vec2(-1.0, -1.0), 0.0).rgb;
    s += textureSampleLevel(src, smp, in.uv + t * vec2(1.0, -1.0), 0.0).rgb;
    s += textureSampleLevel(src, smp, in.uv + t * vec2(-1.0, 1.0), 0.0).rgb;
    s += textureSampleLevel(src, smp, in.uv + t * vec2(1.0, 1.0), 0.0).rgb;
    return vec4<f32>(s / 16.0, 1.0);
}
