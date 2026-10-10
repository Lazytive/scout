/**
 * Advanced mode's keyframes and speed ramps, a port of `klip_edit::advanced`
 * (ADR 0018), checked against `crates/klip-edit/fixtures/advanced/*.json` by
 * `keyframes.test.ts`: the preview's canvas and the export agree on every number.
 */

export type Ease = 'linear' | 'hold';

export interface Keyframe {
  at_ms: number;
  value: number;
  ease: Ease;
}

export interface Transform {
  zoom: number;
  x: number;
  y: number;
}

export interface TransformKey {
  at_ms: number;
  value: Transform;
  ease: Ease;
}

export interface SpeedPoint {
  at_ms: number;
  rate_pct: number;
}

function lerp(a: number, b: number, ta: number, tb: number, t: number): number {
  if (tb <= ta) return b;
  const f = Math.max(0, t - ta) / (tb - ta);
  return Math.fround(a + (b - a) * f);
}

function around<T extends { at_ms: number }>(keys: T[], t: number): [T, T | undefined] | null {
  const first = keys[0];
  if (!first) return null;
  if (t <= first.at_ms) return [first, undefined];
  let i = -1;
  for (let k = keys.length - 1; k >= 0; k--) {
    if (keys[k]!.at_ms <= t) {
      i = k;
      break;
    }
  }
  if (i < 0) return null;
  return [keys[i]!, keys[i + 1]];
}

/** The value at output time `t`; `null` without keys. */
export function valueAt(keys: Keyframe[], t: number): number | null {
  const pair = around(keys, t);
  if (!pair) return null;
  const [a, b] = pair;
  if (a.ease === 'linear' && b) return lerp(a.value, b.value, a.at_ms, b.at_ms, t);
  return a.value;
}

/** The transform at output time `t`; `null` without keys. */
export function transformAt(keys: TransformKey[], t: number): Transform | null {
  const pair = around(keys, t);
  if (!pair) return null;
  const [a, b] = pair;
  if (a.ease === 'linear' && b) {
    return {
      zoom: lerp(a.value.zoom, b.value.zoom, a.at_ms, b.at_ms, t),
      x: lerp(a.value.x, b.value.x, a.at_ms, b.at_ms, t),
      y: lerp(a.value.y, b.value.y, a.at_ms, b.at_ms, t),
    };
  }
  return a.value;
}

function segments(points: SpeedPoint[]): SpeedPoint[] {
  const out: SpeedPoint[] = [];
  if (!points[0] || points[0].at_ms > 0) out.push({ at_ms: 0, rate_pct: 100 });
  for (const p of points) {
    const last = out[out.length - 1];
    if (last && last.at_ms >= p.at_ms) out.pop();
    out.push(p);
  }
  return out;
}

/** Source ms consumed by `outMs` of output (piecewise-constant rates, floor per segment). */
export function sourceOffset(points: SpeedPoint[], outMs: number): number {
  const segs = segments(points);
  let src = 0;
  for (let i = 0; i < segs.length; i++) {
    const p = segs[i]!;
    if (outMs <= p.at_ms) break;
    const next = segs[i + 1];
    const end = next ? Math.min(next.at_ms, outMs) : outMs;
    src += Math.floor(((end - p.at_ms) * p.rate_pct) / 100);
  }
  return src;
}

/** Where a picture lands on the output canvas, output pixels (`compose::Placement`). */
export interface Placement {
  x: number;
  y: number;
  width: number;
  height: number;
}

export type PictureKind = 'video' | 'image' | 'audio' | 'music';

const U32_MAX = 0xffff_ffff;
const I32_MIN = -0x8000_0000;
const I32_MAX = 0x7fff_ffff;
/** `advanced::ZOOM` as Rust holds it (f32 bounds widened to f64). */
const ZOOM_MIN = Math.fround(0.1);
const ZOOM_MAX = 8;

/** `floor(v + 0.5)`, the rounding both sides use (`Math.round` differs on negative halves in Rust). */
export function roundHalfUp(v: number): number {
  return Math.floor(v + 0.5);
}

function clampNum(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

function scaleInto(w: number, h: number, maxW: number, maxH: number, upscale: boolean): [number, number] {
  const fw = Math.max(1, w);
  const fh = Math.max(1, h);
  let scale = Math.min(Math.max(2, maxW) / fw, Math.max(2, maxH) / fh);
  if (!upscale) scale = Math.min(scale, 1);
  const even = (v: number) => {
    const px = clampNum(roundHalfUp(v), 2, U32_MAX);
    return Math.max(2, px - (px % 2));
  };
  return [even(fw * scale), even(fh * scale)];
}

/** `compose::fit_in`: scaled down (never up) to fit, aspect kept, even sides ≥ 2. */
export function fitIn(w: number, h: number, maxW: number, maxH: number): [number, number] {
  return scaleInto(w, h, maxW, maxH, false);
}

/** `compose::fit_contain`: scaled up or down to fit, aspect kept, even sides ≥ 2. */
export function fitContain(w: number, h: number, maxW: number, maxH: number): [number, number] {
  return scaleInto(w, h, maxW, maxH, true);
}

/**
 * `compose::fitted`: a layer's picture size at zoom 1 on a `canvas` (the main clip
 * fills it, another video fits inside it, a picture keeps its size relative to the
 * main clip's `cropW` source pixels).
 */
export function fitted(kind: PictureKind, main: boolean, native: [number, number], cropW: number, canvas: [number, number]): [number, number] {
  const outW = Math.max(2, canvas[0]);
  const outH = Math.max(2, canvas[1]);
  if (main) return [outW, outH];
  if (kind === 'image') {
    const s = outW / Math.max(1, cropW);
    const px = (v: number) => clampNum(roundHalfUp(Math.max(1, v) * s), 1, U32_MAX);
    return fitIn(px(native[0]), px(native[1]), outW, outH);
  }
  return fitContain(native[0], native[1], outW, outH);
}

/** `compose::place`: zoom about the centre, `x`/`y` shift by that fraction of the canvas. */
export function place(t: Transform, fitW: number, fitH: number, outW: number, outH: number): Placement {
  const zoom = Number.isFinite(t.zoom) ? clampNum(Math.fround(t.zoom), ZOOM_MIN, ZOOM_MAX) : 1;
  const shift = (v: number) => (Number.isFinite(v) ? clampNum(Math.fround(v), -1, 1) : 0);
  const w = Math.max(1, roundHalfUp(fitW * zoom));
  const h = Math.max(1, roundHalfUp(fitH * zoom));
  const x = roundHalfUp((outW - w) / 2 + shift(t.x) * outW);
  const y = roundHalfUp((outH - h) / 2 + shift(t.y) * outH);
  return {
    x: clampNum(x, I32_MIN, I32_MAX),
    y: clampNum(y, I32_MIN, I32_MAX),
    width: Math.min(w, U32_MAX),
    height: Math.min(h, U32_MAX),
  };
}

export interface PixelRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * `compose::visible`: the part of a `srcW x srcH` picture placed at `p` that lands on
 * the canvas, as `[source rect, canvas rect]` in whole pixels, or `null`.
 */
export function visible(p: Placement, srcW: number, srcH: number, outW: number, outH: number): [PixelRect, PixelRect] | null {
  if (p.width === 0 || p.height === 0 || srcW === 0 || srcH === 0) return null;
  const axis = (pos: number, len: number, src: number, out: number): [number, number, number, number] | null => {
    const start = pos;
    const end = start + len;
    const d0 = Math.max(start, 0);
    const d1 = Math.min(end, out);
    if (d1 <= d0) return null;
    const toSrc = (d: number) => clampNum(roundHalfUp(((d - start) * src) / len), 0, src);
    let s0 = toSrc(d0);
    let s1 = toSrc(d1);
    if (s1 <= s0) {
      s1 = Math.min(s0 + 1, src);
      s0 = Math.max(0, s1 - 1);
    }
    return [s0, s1 - s0, d0, d1 - d0];
  };
  const ax = axis(p.x, p.width, srcW, outW);
  const ay = axis(p.y, p.height, srcH, outH);
  if (!ax || !ay) return null;
  return [
    { x: ax[0], y: ay[0], width: ax[1], height: ay[1] },
    { x: ax[2], y: ay[2], width: ax[3], height: ay[3] },
  ];
}

export interface Colour {
  brightness: number;
  contrast: number;
  saturation: number;
}

export const IDENTITY_COLOUR: Colour = { brightness: 0, contrast: 1, saturation: 1 };

const LUMA = [0.2126, 0.7152, 0.0722].map(Math.fround);

/** `Colour::sanitized`: clamped; anything not finite is "as recorded". */
export function sanitizedColour(c: Colour): Colour {
  const f = (v: number, lo: number, hi: number, d: number) => (Number.isFinite(v) ? clampNum(Math.fround(v), lo, hi) : d);
  return {
    brightness: f(c.brightness, -1, 1, 0),
    contrast: f(c.contrast, 0, 2, 1),
    saturation: f(c.saturation, 0, 2, 1),
  };
}

export function isIdentityColour(c: Colour): boolean {
  const s = sanitizedColour(c);
  return s.brightness === 0 && s.contrast === 1 && s.saturation === 1;
}

/**
 * `Colour::matrix`: rows `[r, g, b, offset]` on 0…255 values, in f32 like Rust. The
 * preview hands it to an SVG `feColorMatrix` with the offsets divided by 255.
 */
export function colourMatrix(colour: Colour): number[][] {
  const c = sanitizedColour(colour);
  const f = Math.fround;
  const s = c.saturation;
  const k = c.contrast;
  const offset = f(f(128 * f(1 - k)) + f(255 * c.brightness));
  const m: number[][] = [];
  for (let i = 0; i < 3; i++) {
    const row: number[] = [];
    for (let j = 0; j < 3; j++) {
      const w = LUMA[j]!;
      const sat = f(f(w * f(1 - s)) + (i === j ? s : 0));
      row.push(f(k * sat));
    }
    row.push(offset);
    m.push(row);
  }
  return m;
}

/** `colourMatrix` as an SVG `feColorMatrix type="matrix"` value (4 x 5, offsets / 255). */
export function svgColourMatrix(colour: Colour): string {
  const m = colourMatrix(colour);
  const rows = m.map((r) => [r[0], r[1], r[2], 0, r[3]! / 255].join(' '));
  return [...rows, '0 0 0 1 0'].join(' ');
}

/** Output ms it takes to play `srcLen` source ms. */
export function outputLength(points: SpeedPoint[], srcLen: number): number {
  const segs = segments(points);
  let src = 0;
  let out = 0;
  for (let i = 0; i < segs.length; i++) {
    const p = segs[i]!;
    const rate = Math.max(1, p.rate_pct);
    const next = segs[i + 1];
    if (!next) {
      const left = srcLen - src;
      return out + Math.floor((left * 100 + rate - 1) / rate);
    }
    const len = next.at_ms - p.at_ms;
    const consumed = Math.floor((len * rate) / 100);
    if (src + consumed >= srcLen) {
      const left = srcLen - src;
      return out + Math.floor((left * 100 + rate - 1) / rate);
    }
    src += consumed;
    out = next.at_ms;
  }
  return out;
}
