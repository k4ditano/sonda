// The water Sonda sits in: deep at the bottom, lighter towards the surface,
// light coming down in slanted shafts, and a faint net of caustics near the
// top. It never reads the time: it moves only while `tide` (a.x) travels
// —a folder entered, a search— and rests the rest of the time.
fn shafts(p: vec2<f32>, tide: f32) -> f32 {
    // slanted bands, wider towards the bottom, three of them crossing
    let x = p.x + p.y * 0.35;
    let a = pow(0.5 + 0.5 * sin(x * 0.0105 + tide * 0.9), 10.0);
    let b = pow(0.5 + 0.5 * sin(x * 0.0067 - 1.7 + tide * 0.55), 14.0);
    let c = pow(0.5 + 0.5 * sin(x * 0.0151 + 2.3 - tide * 0.4), 18.0);
    return a * 0.55 + b * 0.8 + c * 0.35;
}

fn caustic(p: vec2<f32>, tide: f32) -> f32 {
    var q = p * 0.018;
    var v = 0.0;
    for (var i = 0; i < 3; i = i + 1) {
        let fi = f32(i);
        q = vec2<f32>(q.x + sin(q.y * 1.3 + tide + fi), q.y + cos(q.x * 1.1 - tide * 0.8 + fi * 1.7));
        v = v + abs(sin(q.x + q.y));
    }
    // thin bright lines where the waves cross
    return pow(1.0 - clamp(v / 3.0, 0.0, 1.0), 6.0);
}

fn shade(s: Shader) -> vec4<f32> {
    let tide = s.a.x;
    let depth = clamp(s.uv.y, 0.0, 1.0);
    // colour: color (deep) at the bottom, color2 (shallow) at the top
    var col = mix(s.color2.rgb, s.color.rgb, smoothstep(0.0, 1.0, pow(depth, 0.7)));
    // a glow where the surface is, off-centre to the right
    let glow = exp(-pow(length((s.uv - vec2<f32>(0.62, -0.08)) * vec2<f32>(1.0, 1.6)), 2.0) * 3.2);
    col = col + vec3<f32>(0.10, 0.17, 0.15) * glow;
    // light shafts fading with depth
    let fade = exp(-depth * 2.6);
    col = col + vec3<f32>(0.20, 0.32, 0.28) * shafts(s.pos, tide) * fade * 0.16;
    // caustics only near the surface
    col = col + vec3<f32>(0.35, 0.55, 0.48) * caustic(s.pos, tide) * exp(-depth * 7.0) * 0.10;
    return vec4<f32>(col, 1.0);
}
