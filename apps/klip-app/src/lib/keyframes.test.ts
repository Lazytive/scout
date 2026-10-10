/// <reference types="node" />
import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import { type Keyframe, type SpeedPoint, type TransformKey, outputLength, sourceOffset, transformAt, valueAt } from './keyframes';

const dir = new URL('../../../../crates/klip-edit/fixtures/advanced/', import.meta.url);
const read = (name: string) => JSON.parse(readFileSync(new URL(name, dir), 'utf8')) as Record<string, unknown>;

describe('shared fixtures (crates/klip-edit/fixtures/advanced)', () => {
  it('keyframes evaluate like klip-edit', () => {
    const fx = read('keyframes.json');
    const keys = fx.keys as Keyframe[];
    for (const [t, want] of fx.expect as [number, number][]) {
      expect(valueAt(keys, t), `t=${t}`).toBeCloseTo(want, 5);
    }
    const tkeys = fx.transform_keys as TransformKey[];
    for (const [t, [zoom, x, y]] of fx.transform_expect as [number, [number, number, number]][]) {
      const got = transformAt(tkeys, t)!;
      expect(got.zoom, `t=${t} zoom`).toBeCloseTo(zoom, 5);
      expect(got.x, `t=${t} x`).toBeCloseTo(x, 5);
      expect(got.y, `t=${t} y`).toBeCloseTo(y, 5);
    }
    expect(valueAt([], 5)).toBeNull();
  });

  it('speed ramps map like klip-edit', () => {
    const fx = read('ramp.json');
    const points = fx.points as SpeedPoint[];
    for (const [out, want] of fx.source_offset as [number, number][]) {
      expect(sourceOffset(points, out), `out=${out}`).toBe(want);
    }
    for (const [src, want] of fx.output_length as [number, number][]) {
      expect(outputLength(points, src), `src=${src}`).toBe(want);
    }
    expect(outputLength([], 1234)).toBe(1234);
  });
});
