// A glyph of grains: five by five, each one lit or not (Calima's grains,
// what a kind of file looks like). The mask comes in two numbers: rows 0–2
// in a.x (bits 0–14) and rows 3–4 in a.y (bits 0–9), a row's first grain on
// its lowest bit. a.z: a grain's place (grain and gap), a.w: the grain;
// b.x: how round (0 square, 1 a dot); b.y, b.z: how much a lit and an unlit
// one show. color: lit, color2: unlit.
fn grain_box(p: vec2<f32>, half: f32, r: f32) -> f32 {
    let q = abs(p) - vec2<f32>(half - r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn shade(s: Shader) -> vec4<f32> {
    let cell = max(s.a.z, 1.0);
    let c = floor(s.pos / cell);
    if (c.x < 0.0 || c.y < 0.0 || c.x > 4.0 || c.y > 4.0) {
        return vec4<f32>(0.0);
    }
    let k = u32(c.y) * 5u + u32(c.x);
    var lit = false;
    if (k < 15u) {
        lit = ((u32(s.a.x) >> k) & 1u) == 1u;
    } else {
        lit = ((u32(s.a.y) >> (k - 15u)) & 1u) == 1u;
    }
    let half = s.a.w * 0.5;
    let d = grain_box(s.pos - (c + 0.5) * cell, half, half * clamp(s.b.x, 0.0, 1.0));
    let cover = clamp(0.5 - d * s.scale, 0.0, 1.0);
    if (lit) {
        return vec4<f32>(s.color.rgb, cover * s.b.y);
    }
    return vec4<f32>(s.color2.rgb, cover * s.b.z);
}
