// A row of grains, some of them lit: how big a file is, how full a disk,
// how far the reading or a copy has gone. a.x: how many, a.y: how many lit
// (the last one, partly: its fraction is how much it shows); a.z: a grain's
// place (grain and gap), a.w: the grain; b.x: how round; b.y, b.z: how much
// a lit and an unlit one show. color: lit, color2: unlit.
fn grain_box2(p: vec2<f32>, half: f32, r: f32) -> f32 {
    let q = abs(p) - vec2<f32>(half - r);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn shade(s: Shader) -> vec4<f32> {
    let cell = max(s.a.z, 1.0);
    let i = floor(s.pos.x / cell);
    if (i < 0.0 || i >= s.a.x) {
        return vec4<f32>(0.0);
    }
    let half = s.a.w * 0.5;
    let centre = vec2<f32>((i + 0.5) * cell, s.size.y * 0.5);
    let d = grain_box2(s.pos - centre, half, half * clamp(s.b.x, 0.0, 1.0));
    let cover = clamp(0.5 - d * s.scale, 0.0, 1.0);
    let on = clamp(s.a.y - i, 0.0, 1.0);
    let lit = vec4<f32>(s.color.rgb, cover * s.b.y);
    let unlit = vec4<f32>(s.color2.rgb, cover * s.b.z);
    return mix(unlit, lit, on);
}
