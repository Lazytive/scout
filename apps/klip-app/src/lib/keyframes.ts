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
