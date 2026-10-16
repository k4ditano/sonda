// A job card filling with water as the job goes (a.x, 0 → 1): a surface with
// two waves on it and a brighter line where the light catches it.
fn shade(s: Shader) -> vec4<f32> {
    let level = clamp(s.a.x, 0.0, 1.0);
    let x = s.pos.x;
    let surface = s.size.y * (1.0 - level)
        + sin(x * 0.045 + s.time * 2.2) * 2.2
        + sin(x * 0.11 - s.time * 3.1) * 1.1;
    let under = smoothstep(surface - 0.8, surface + 0.8, s.pos.y);
    let line = exp(-pow((s.pos.y - surface) / 1.4, 2.0));
    let deeper = clamp((s.pos.y - surface) / max(s.size.y, 1.0), 0.0, 1.0);
    let a = under * (0.16 + 0.10 * deeper) + line * 0.45 * step(0.001, level);
    return vec4<f32>(mix(s.color.rgb, s.color2.rgb, deeper), a);
}
