// Kabukichō Gate. The prelude (ukiyo-generated) gives us `u` (built-ins,
// params and bindings), `smp`, `smp_repeat`, `t_base`, `t_depth`,
// `t_emissive` (r bulbs, g signs, b billboards, a sign id), `t_regions`
// (r sky, g street, b people, a gate), `image_uv()` and `aces()`.

const FOCAL: f32 = 0.55;       // depth that stays put when the camera moves
const PARALLAX: vec2<f32> = vec2<f32>(0.022, 0.012);

// ---------------------------------------------------------------- noise

// Integer hashes (PCG): no sin(), cheap on GPUs and on software rasterisers.
fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn hash11(n: f32) -> f32 {
    return f32(pcg(bitcast<u32>(n))) * (1.0 / 4294967295.0);
}

fn hash21(p: vec2<f32>) -> f32 {
    let b = bitcast<vec2<u32>>(p);
    return f32(pcg(b.x ^ pcg(b.y))) * (1.0 / 4294967295.0);
}

fn noise2(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let w = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2(1.0, 0.0));
    let c = hash21(i + vec2(0.0, 1.0));
    let d = hash21(i + vec2(1.0, 1.0));
    return mix(mix(a, b, w.x), mix(c, d, w.x), w.y);
}

fn fbm(p: vec2<f32>) -> f32 {
    var s = 0.0;
    var a = 0.5;
    var q = p;
    for (var i = 0; i < 3; i++) {
        s += a * noise2(q);
        q = q * 2.03 + vec2(17.1, 9.7);
        a *= 0.5;
    }
    return s;
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3(0.2126, 0.7152, 0.0722));
}

// ---------------------------------------------------------------- camera

// The camera offset (image uv per unit of depth difference from FOCAL):
// cursor parallax, the workspace drift, a slow breathing sway, and a jolt
// when a build fails.
fn camera() -> vec2<f32> {
    let sway = vec2(sin(u.time * 0.071) * 0.25, sin(u.time * 0.053 + 1.3) * 0.18) * (1.0 - u.still);
    let jolt_t = u.failed_age;
    let jolt = vec2(sin(jolt_t * 70.0), cos(jolt_t * 53.0)) * exp(-jolt_t * 9.0) * 0.12 * step(jolt_t, 1.0);
    return (vec2(u.cam_x, u.cam_y) + sway + jolt) * PARALLAX;
}

// Find the image point that lands on screen point `p` after parallax:
// p = q + cam * (depth(q) - FOCAL). Fixed-point iterations on a blurred
// depth (a blurred disparity tears less at edges).
fn parallax(p: vec2<f32>, cam: vec2<f32>) -> vec2<f32> {
    var q = p;
    for (var i = 0; i < 3; i++) {
        let d = textureSampleLevel(t_depth, smp, q, 1.5).r;
        q = p - cam * (d - FOCAL);
    }
    return q;
}

// ---------------------------------------------------------------- rain

// One sheet of rain streaks at depth `layer_depth` (0 far .. 1 near).
// Returns streak coverage at screen uv.
fn rain_layer(uv: vec2<f32>, cam: vec2<f32>, layer: f32, layer_depth: f32, density: f32) -> f32 {
    let aspect = u.resolution.x / u.resolution.y;
    // Nearer sheets: bigger, sparser, faster, softer.
    let ld = layer_depth;
    let cols = mix(320.0, 34.0, pow(ld, 0.7));
    let rows = mix(16.0, 2.2, pow(ld, 0.7));
    let speed = mix(0.75, 2.8, ld);   // screen heights per second
    var p = uv - cam * (ld - FOCAL) * 1.4;
    p.x = p.x * aspect + p.y * u.wind * mix(0.8, 1.2, ld);
    var q = vec2(p.x * cols / aspect, (p.y - u.time * speed) * rows);
    // Every column falls at its own offset.
    let col = floor(q.x);
    q.y += hash11(col * 1.37 + layer * 17.0) * 7.0;
    let cell = floor(q);
    let f = fract(q);
    let h = hash21(cell + layer * 31.7);
    if (h > density) {
        return 0.0;
    }
    let x = 0.5 + (hash21(cell + 7.3) - 0.5) * 0.7;
    // One pixel, in cells; near drops are out of focus, so wider and fainter.
    let px = cols / u.resolution.x;
    let w = px * mix(0.5, 3.0, ld * ld);
    let len = mix(0.45, 0.85, hash21(cell + 2.1));
    let y0 = hash21(cell + 4.4) * (1.0 - len);
    let along = (f.y - y0) / len;
    // Motion-blurred drop: a long faint tail, the brightest at the head.
    let body = smoothstep(0.0, 0.9, along) * (1.0 - smoothstep(0.92, 1.0, along));
    let across = 1.0 - smoothstep(w * 0.5, w * 0.5 + px * mix(1.0, 3.0, ld), abs(f.x - x));
    let bright = mix(0.35, 1.0, hash21(cell + 9.9)) * mix(1.0, 0.45, ld);
    return body * across * bright;
}

// ---------------------------------------------------------------- scene

@fragment
fn fs_scene(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    let cam = camera();
    let p0 = image_uv(uv);
    let p = parallax(p0, cam);

    var base = textureSample(t_base, smp, p).rgb;
    let depth = textureSampleLevel(t_depth, smp, p, 0.0).r;
    let em = textureSample(t_emissive, smp, p);
    let rg = textureSample(t_regions, smp, p);
    let calm = 1.0 - u.still;
    // The street's own light, very blurred: what lights the air and the rain.
    let glow = textureSampleLevel(t_base, smp, p, 7.0).rgb;
    let glow_wide = textureSampleLevel(t_base, smp, p0, 9.0).rgb;

    // ------------------------------------------------ signs
    let id = em.a;
    let signs = max(em.g, em.b);
    let sign_on = step(0.001, id);
    // Some shops close as the night gets deeper.
    let closed = step(hash11(id * 91.3), u.closing * smoothstep(0.2, 0.9, u.night)) * sign_on;
    // Flicker, the way neon fails: a sign starts stuttering for a quarter
    // second (a burst of fast on/off), then holds. How often depends on
    // `flicker` (typing); CPU load adds a buzz on some signs.
    let slow = floor(u.time * 4.0 + id * 13.0);
    let chance = clamp(u.flicker, 0.0, 1.5) * 0.3 * calm;
    let burst = step(1.0 - chance, hash21(vec2(id * 113.0, slow)));
    let fast = floor(u.time * 30.0);
    let off = burst * step(0.5, hash21(vec2(id * 71.0, fast)));
    let depth_of_drop = mix(0.08, 0.45, hash21(vec2(id * 7.0, slow + 3.0)));
    let drop = off;
    let buzz = 1.0 + u.buzz * 0.18 * calm * sin(u.time * 113.0 + id * 300.0) * step(0.55, hash11(id * 17.0));
    var sign_gain = mix(1.0, depth_of_drop, drop) * buzz;
    sign_gain = mix(sign_gain, 0.07, closed);

    // Billboards pulse with the music.
    let music = (u.bass * 0.9 + u.beat * 0.7) * calm;
    let board_gain = 1.0 + music * 0.35;

    // Emissive boost: signs are light sources, brighter than the photo can hold.
    // The photo stays the photo: walls a little darker for mood, lit signs
    // as shot (they feed the bloom), modulated by flicker and music.
    let lit_soft = textureSampleLevel(t_emissive, smp, p, 2.5);
    let sign_w = clamp(max(lit_soft.g, lit_soft.b) * 1.5 + lit_soft.r, 0.0, 1.0);
    var col = base * mix(0.82, 1.0, sign_w);
    col = mix(col, base * sign_gain * 1.08, clamp(em.g, 0.0, 1.0));
    // The billboards throw light on the beat (HDR: the bloom halo pulses),
    // and the whole street's neon swells a little with the bass.
    col = mix(col, col * board_gain + base * music * 0.45, em.b);
    col *= 1.0 + u.bass * 0.12 * calm * sign_w;

    // ------------------------------------------------ the gate
    let gate = rg.a;
    let bulb = smoothstep(0.5, 0.9, em.r);
    let panel = em.r * (1.0 - bulb);
    // A slow marquee chase along the arch.
    let chase = 1.0 + u.chase * 0.55 * calm * sin(p.x * 90.0 - u.time * 4.0 + p.y * 30.0);
    // Now and then a bulb sags for a moment, like old filament bulbs do.
    let bcell = floor(p * vec2(150.0, 95.0));
    let sag = step(0.985, hash21(bcell + floor(u.time * 3.0 + hash21(bcell) * 9.0))) * calm;
    var bulb_col = base * (2.2 * chase) * (1.0 - 0.55 * sag);

    // Pulse strength (gain, intensity, on/off) without the bus's decay:
    // the shape comes from the age.
    let fa = min(u.failed_age, 8.0);
    let fail_gain = min(u.failed * exp(fa / 0.5), 2.0);
    let pa = min(u.passed_age, 8.0);
    let pass_gain = min(u.passed * exp(pa / 0.5), 2.0);
    // Build failed: the gate strobes red, then glows and fades.
    let strobe = select(1.0, 0.45 + 0.55 * step(0.38, fract(fa * 7.5)), fa < 0.62);
    let fail = fail_gain * exp(-fa / 1.6) * strobe;
    let fail_col = u.fail_color.rgb;
    // Build passed: green sweeps across the arch left to right, then fades.
    let sweep = smoothstep(p.x - 0.12, p.x, pa * 2.4 - 0.05);
    let pass_k = pass_gain * exp(-pa / 1.9) * sweep;
    let pass_col = u.pass_color.rgb;

    let flash = clamp(fail, 0.0, 1.0);
    let green = clamp(pass_k, 0.0, 1.0) * (1.0 - flash);
    let bulb_luma = luma(base) + 0.3;
    // The first half second strobes white-hot, then it burns red.
    let hot = select(0.0, step(0.38, fract(fa * 7.5)), fa < 0.62);
    let fail_bulb = mix(fail_col * 12.0, vec3(9.0, 5.0, 5.0), hot * 0.7) * bulb_luma;
    bulb_col = mix(bulb_col, fail_bulb, flash);
    bulb_col = mix(bulb_col, pass_col * bulb_luma * 8.0, green);
    col = mix(col, bulb_col, bulb);
    // The whole gate (arch, pillars, lightbox) takes the colour.
    let gate_body = gate * (1.0 - bulb);
    let body_l = luma(col);
    // The lightbox inverts: red on black, like an alarm panel.
    let alarm = fail_col * (pow(1.0 - clamp(body_l, 0.0, 1.0), 2.0) * 3.0 + 0.04);
    col = mix(col, alarm, gate_body * flash * 0.95);
    col = mix(col, pass_col * (body_l * 2.2 + 0.04), gate_body * green * 0.85);
    // The street holds its breath.
    col *= 1.0 - (0.55 * flash + 0.18 * green) * (1.0 - gate);
    let tint = fail_col * fail + pass_col * pass_k * (1.0 - flash);

    // ------------------------------------------------ time of day
    let sky = rg.r;
    // Dusk: the sky goes deep blue to violet at the horizon, the city cools.
    let sky_y = clamp(p.y / 0.6, 0.0, 1.0);
    let twilight = mix(vec3(0.012, 0.03, 0.13), vec3(0.20, 0.07, 0.17), sky_y * sky_y);
    let evening = clamp(u.dusk + u.day * 0.6, 0.0, 1.0);
    col = mix(col, max(col, twilight * 1.5), sky * evening);
    // Blue hour in the shadows.
    col += vec3(0.004, 0.009, 0.03) * evening * (1.0 - sign_w);
    // Clouds drifting over the sky, lit from below by the street.
    if (sky > 0.01) {
        let cloud = fbm(vec2(p.x * 3.0 + u.time * 0.012, p.y * 6.0 - u.time * 0.004));
        col += sky * smoothstep(0.45, 0.95, cloud) * glow_wide * (0.35 + 0.4 * evening) * (1.0 - 0.5 * u.night);
    }

    // ------------------------------------------------ air
    // Wet haze down the street: far things fade into the signs' light.
    let far = 1.0 - depth;
    let wet = u.rain_amount;
    let haze = u.haze * (0.35 + 0.65 * wet) * (0.55 + 0.45 * u.night);
    let air = glow * 0.55 + vec3(0.006, 0.008, 0.016);
    col = mix(col, air, clamp(haze * far * far * 0.45, 0.0, 0.6));
    // Mist drifting down the street, lit by the signs.
    let mist_reach = far * smoothstep(0.15, 0.6, p.y) * u.haze * (0.45 + 0.55 * wet);
    if (mist_reach > 0.02) {
        let mp = vec2(p.x * 2.2 - u.time * 0.018, p.y * 3.4 + u.time * 0.006);
        let mn = fbm(mp + vec2(sin(mp.y * 1.7 + u.time * 0.05), 0.0) * 0.6);
        col += (glow * 0.7 + vec3(0.01, 0.012, 0.02)) * smoothstep(0.42, 0.85, mn) * mist_reach * 0.55;
    }
    // Red spill from the gate on everything around it.
    let gate_light = textureSampleLevel(t_regions, smp, p, 6.0).a;
    col += gate_light * tint * 0.6 * (1.0 - gate);

    // ------------------------------------------------ rain
    // Five sheets, far to near. A drop shows only where the scene behind it
    // is farther than the sheet, so rain falls behind the gate and in front.
    let amount = wet * mix(1.0, 0.0, u.still * 0.7);
    var rain = vec3(0.0);
    let depths = array<f32, 6>(0.06, 0.2, 0.38, 0.6, 0.8, 0.97);
    let rain_glow = glow * 1.6 + glow_wide * 1.4;
    let rain_light = mix(vec3(luma(rain_glow)), rain_glow, 0.55) + vec3(0.025, 0.03, 0.045);
    for (var i = 0; i < 6; i++) {
        let ld = depths[i];
        let density = amount * mix(0.85, 0.3, ld);
        let s = rain_layer(uv, cam, f32(i), ld, density);
        let visible = smoothstep(ld + 0.04, ld - 0.02, depth);
        rain += s * visible * rain_light;
    }
    col += rain;
    // Drops bursting on the top of the arch and the lightbox.
    let up = textureSampleLevel(t_regions, smp, p - vec2(0.0, 0.006), 0.0).a;
    let top = clamp(gate - up, 0.0, 1.0);
    if (top > 0.01) {
        let sc = vec2(uv.x * 260.0, uv.y * 140.0);
        let sid = floor(sc);
        let rate = 1.5 + 5.0 * amount;
        let ph = fract(u.time * rate * mix(0.6, 1.4, hash21(sid)) + hash21(sid + 3.1));
        let on = step(1.0 - amount * 0.55, hash21(sid + floor(u.time * rate) * 0.37 + 9.0));
        let sf = fract(sc) - vec2(0.5, 0.5 + ph * 0.6);
        let dot_ = smoothstep(0.32, 0.0, length(sf * vec2(1.0, 0.7))) * (1.0 - ph);
        col += top * on * dot_ * (rain_light * 1.4 + vec3(0.2)) * calm;
    }
    // Heavy rain greys the air out a little.
    col = mix(col, rain_light * 0.18, clamp((amount - 0.5) * 0.25, 0.0, 0.12));

    // ------------------------------------------------ night grade
    // The dead of night: the walls go dark, the light goes cold, mist.
    let n = u.night;
    col *= mix(1.0, mix(0.5, 0.9, sign_w), n);
    col = mix(col, col * vec3(0.82, 0.9, 1.15), n * 0.7);
    let mist = vec3(0.012, 0.016, 0.03) + glow_wide * 0.25;
    col = mix(col, mist, n * clamp(far * 0.55, 0.0, 0.5));
    // Stillness (the lock): darker and colder.
    col = mix(col, col * vec3(0.72, 0.78, 0.92), u.still * 0.6);
    return vec4(col * u.exposure, 1.0);
}

// ---------------------------------------------------------------- post

// Lens drops: (offset to sample through the drop, coverage, highlight).
fn lens_drops(uv: vec2<f32>) -> vec4<f32> {
    let aspect = u.resolution.x / u.resolution.y;
    let amount = u.lens * clamp(u.rain_amount * 1.6, 0.0, 1.0);
    if (amount <= 0.0) {
        return vec4(0.0);
    }
    var out = vec4(0.0);
    let grid = vec2(9.0, 5.0);
    let q = uv * grid;
    let base = floor(q);
    // A drop sits in its cell and may slide down into the next one: look at
    // this cell and the one above.
    for (var j = -1; j <= 0; j++) {
        let cell = base + vec2(0.0, f32(j));
        let h = hash21(cell + 0.5);
        // Drops come and go: each lives for a while, then slides off.
        let life = 9.0 + h * 14.0;
        let k = floor((u.time + h * 50.0) / life);
        let age = fract((u.time + h * 50.0) / life);
        let hk = hash21(cell + k * 1.7);
        if (hk > amount * 0.9) {
            continue;
        }
        let r = mix(0.012, 0.032, pow(hash21(cell + k + 4.0), 2.0));
        let slide = max(age - 0.6, 0.0) * 2.5 * hash21(cell + k + 7.0);
        let c = cell + vec2(0.2 + 0.6 * hash21(cell + k + 2.0), hash21(cell + k + 3.0) * 0.7 + slide);
        var d = (q - c) / grid * vec2(aspect, 1.0);
        d.y *= mix(1.0, 0.8, slide);
        let dist = length(d);
        let cov = smoothstep(r, r * 0.86, dist) * smoothstep(0.0, 0.08, age) * (1.0 - smoothstep(0.92, 1.0, age));
        if (cov > out.z) {
            let n = d / r;
            let hl = smoothstep(0.22, 0.0, length(n - vec2(-0.35, -0.45))) * cov;
            let rim = smoothstep(0.7, 1.0, length(n));
            out = vec4(n * r * 3.0 / vec2(aspect, 1.0), cov * (1.0 - 0.35 * rim), hl * 0.25);
        }
    }
    return out;
}

@fragment
fn fs_post(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.uv;
    // A touch of lateral chromatic aberration towards the edges.
    let d = uv - 0.5;
    let ca = d * dot(d, d) * 0.006;
    var c = vec3(
        textureSample(t_scene, smp, uv + ca).r,
        textureSample(t_scene, smp, uv).g,
        textureSample(t_scene, smp, uv - ca).b,
    );
    let b0 = textureSample(t_bloom, smp, uv).rgb;
    // Raindrops on the lens: a few, refracting the street upside down.
    let lens = lens_drops(uv);
    if (lens.z > 0.0) {
        let src = clamp(uv - lens.xy, vec2(0.0), vec2(1.0));
        // Through the drop: the scene flipped and a little soft.
        let through = textureSample(t_scene, smp, src).rgb * 0.75 + textureSample(t_bloom, smp, src).rgb * 0.35;
        c = mix(c, through, lens.z * 0.8);
        // A dark rim where the drop's edge bends light away, and a glint.
        c += vec3(0.5, 0.5, 0.55) * lens.w;
    }
    let b = b0;
    // Bloom with a warm halation tint, like film.
    c += b * vec3(1.0, 0.82, 0.78) * 0.32 * u.bloom;
    // Saturation.
    let l = luma(c);
    c = mix(vec3(l), c, u.saturation);
    // Highlight roll-off: identity below 0.75, smooth shoulder above.
    let t = 0.75;
    let over = max(c - t, vec3(0.0));
    c = min(c, vec3(t)) + (1.0 - t) * (1.0 - exp(-over / (1.0 - t)));
    // Vignette.
    let v = 1.0 - dot(d * vec2(1.1, 1.4), d * vec2(1.1, 1.4)) * 0.9 * u.vignette;
    c *= clamp(v, 0.0, 1.0);
    // Grain (static while still).
    let g = hash21(uv * u.resolution + fract(u.time * 7.13) * 100.0 * (1.0 - u.still)) - 0.5;
    c += g * 0.025 * u.grain;
    return vec4(max(c, vec3(0.0)), 1.0);
}
