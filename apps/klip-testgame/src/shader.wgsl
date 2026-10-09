// Klip test game: barcode band (see klip-testkit::barcode), flash, moving bar, checker.

struct U {
    // x: 32-bit code, y: flash (0/1), z: frame counter, w: cell count
    bits: vec4<u32>,
    // band x0, y0, x1, y1 as fractions of the frame
    band: vec4<f32>,
    // margin, width, height, bar position (0..1)
    misc: vec4<f32>,
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

fn gray(v: f32) -> vec4<f32> {
    return vec4<f32>(v, v, v, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
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
