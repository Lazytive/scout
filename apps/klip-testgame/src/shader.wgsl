// Klip test game: barcode band (see klip-testkit::barcode), flash, moving bar, checker.

struct U {
    // x: 32-bit code, y: flash (0/1), z: frame counter, w: cell count
    bits: vec4<u32>,
    // band x0, y0, x1, y1 as fractions of the frame
    band: vec4<f32>,
    // margin, width, height, bar position (0..1)
    misc: vec4<f32>,
    // hdr (0/1), paper white in scRGB units, patches (0/1), patch count
    extra: vec4<f32>,
    // patch row x0, y0, x1, y1 as fractions of the frame
    row: vec4<f32>,
    // patch output values (already in the swapchain's encoding), 8 slots
    p0: vec4<f32>,
    p1: vec4<f32>,
    // x: extra loop steps per pixel (--gpu-load)
    load: vec4<u32>,
};

@group(0) @binding(0) var<uniform> u: U;

@vertex
fn vs_main(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(i & 2u) * 2.0 - 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

fn lit(cell: u32) -> bool {
    if (cell == 0u) {
        return true;
    }
    if (cell == 1u || cell >= 34u) {
        return false;
    }
    let bit = 33u - cell;
    return ((u.bits.x >> bit) & 1u) == 1u;
}

fn srgb_to_linear(v: f32) -> f32 {
    if (v <= 0.04045) {
        return v / 12.92;
    }
    return pow((v + 0.055) / 1.055, 2.4);
}

// An SDR code value (0..1). In HDR mode it becomes linear scRGB at paper white.
fn gray(v: f32) -> vec4<f32> {
    if (u.extra.x > 0.5) {
        let l = srgb_to_linear(v) * u.extra.y;
        return vec4<f32>(l, l, l, 1.0);
    }
    return vec4<f32>(v, v, v, 1.0);
}

fn raw(v: f32) -> vec4<f32> {
    return vec4<f32>(v, v, v, 1.0);
}

fn patch_value(i: u32) -> f32 {
    if (i < 4u) {
        return u.p0[i];
    }
    return u.p1[i - 4u];
}

// GPU work that the compiler can't drop (its result feeds a branch that never fires).
fn burn(pos: vec4<f32>, n: u32) -> f32 {
    var a = pos.x * 0.001 + pos.y * 0.0007;
    for (var i = 0u; i < n; i = i + 1u) {
        a = fract(sin(a * 12.9898 + f32(i)) * 43758.5453);
    }
    return a;
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    if (u.load.x > 0u && burn(pos, u.load.x) > 2.0) {
        return vec4<f32>(1.0, 0.0, 0.0, 1.0);
    }
    return shade(pos);
}

fn shade(pos: vec4<f32>) -> vec4<f32> {
    let w = u.misc.y;
    let h = u.misc.z;
    let px = floor(pos.x);
    let py = floor(pos.y);
    let band_y0 = floor(u.band.y * h);
    let band_y1 = floor(u.band.w * h);
    if (py >= band_y0 - 2.0 && py < band_y1 + 2.0) {
        let fx = px - u.band.x * w;
        let fy = py - u.band.y * h;
        let band_h = (u.band.w - u.band.y) * h;
        let cw = (u.band.z - u.band.x) * w / f32(u.bits.w);
        if (fx < 0.0 || fy < 0.0 || fy >= band_h) {
            return gray(0.0);
        }
        let cell = u32(floor(fx / cw));
        let lx = fx / cw - f32(cell);
        let ly = fy / band_h;
        let m = u.misc.x;
        if (lx >= m && lx <= 1.0 - m && ly >= m && ly <= 1.0 - m && lit(cell)) {
            return gray(1.0);
        }
        return gray(0.0);
    }
    if (u.extra.z > 0.5) {
        let rx = px / w;
        let ry = py / h;
        if (rx >= u.row.x && rx < u.row.z && ry >= u.row.y && ry < u.row.w) {
            let n = u32(u.extra.w);
            let i = min(u32(floor((rx - u.row.x) / (u.row.z - u.row.x) * f32(n))), n - 1u);
            return raw(patch_value(i));
        }
    }
    if (u.bits.y == 1u) {
        return gray(1.0);
    }
    if (abs(px - floor(u.misc.w * w)) < floor(w / 50.0)) {
        return gray(220.0 / 255.0);
    }
    let shift = f32((u.bits.z * 4u) % 4096u);
    let cx = u32(floor((px + shift) / 32.0));
    let cy = u32(floor(py / 32.0));
    if ((cx + cy) % 2u == 0u) {
        return gray(60.0 / 255.0);
    }
    return gray(90.0 / 255.0);
}
