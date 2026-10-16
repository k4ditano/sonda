// A sonar ping: a ring going out from where you asked (a.y, a.z), as far as
// a.x says (0 → 1), with a wake behind it and the echo of a sweep line.
// Driven by a spring: it is painted only while the ping travels.
fn shade(s: Shader) -> vec4<f32> {
    let t = clamp(s.a.x, 0.0, 1.0);
    let o = vec2<f32>(s.a.y, s.a.z);
    let d = length(s.pos - o);
    let reach = length(s.size) * 1.05;
    let r = t * reach;
    // the front: thin and bright; the wake: wide and faint, inside it
    let front = exp(-pow((d - r) / 2.2, 2.0));
    let wake = exp(-pow((d - r) / 60.0, 2.0)) * step(d, r);
    // a second, weaker ring a little behind
    let r2 = max(r - 90.0, 0.0);
    let second = exp(-pow((d - r2) / 1.6, 2.0)) * 0.45 * step(1.0, r2);
    let fade = (1.0 - t) * (1.0 - t);
    let a = (front * 0.75 + wake * 0.10 + second) * fade * s.a.w;
    return vec4<f32>(s.color.rgb, clamp(a, 0.0, 1.0));
}
