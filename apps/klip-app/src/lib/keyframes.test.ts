/// <reference types="node" />
import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import {
  type Colour,
  type Keyframe,
  type PictureKind,
  type Placement,
  type SpeedPoint,
  type Transform,
  type TransformKey,
  colourMatrix,
  fitContain,
  fitIn,
  fitted,
  outputLength,
  place,
  sourceOffset,
  svgColourMatrix,
  transformAt,
  valueAt,
  visible,
} from './keyframes';

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

describe('placement fixture (crates/klip-edit/fixtures/advanced/placement.json)', () => {
  const fx = read('placement.json') as Record<string, Record<string, unknown>[]>;

  it('fits like klip-edit', () => {
    for (const c of fx.fit!) {
      const [w, h, mw, mh] = c.args as number[];
      const want = c.expect as { fit_in: number[]; fit_contain: number[] };
      expect(fitIn(w!, h!, mw!, mh!), JSON.stringify(c.args)).toEqual(want.fit_in);
      expect(fitContain(w!, h!, mw!, mh!), JSON.stringify(c.args)).toEqual(want.fit_contain);
    }
    for (const c of fx.fitted!) {
      const got = fitted(
        c.kind as PictureKind,
        c.main as boolean,
        c.native as [number, number],
        c.crop_w as number,
        c.canvas as [number, number],
      );
      expect(got, JSON.stringify(c)).toEqual(c.expect);
    }
  });

  it('places and clips like klip-edit', () => {
    for (const c of fx.place!) {
      const [fw, fh] = c.fit as number[];
      const [ow, oh] = c.out as number[];
      expect(place(c.t as Transform, fw!, fh!, ow!, oh!), JSON.stringify(c)).toEqual(c.expect);
    }
    for (const c of fx.visible!) {
      const [sw, sh] = c.src as number[];
      const [ow, oh] = c.out as number[];
      expect(visible(c.p as Placement, sw!, sh!, ow!, oh!), JSON.stringify(c)).toEqual(c.expect);
    }
  });

  it('grades like klip-edit', () => {
    for (const c of fx.colour!) {
      const got = colourMatrix(c.c as Colour).flat();
      const want = (c.expect as number[][]).flat();
      got.forEach((v, i) => expect(v, `${JSON.stringify(c.c)}[${i}]`).toBeCloseTo(want[i]!, 6));
    }
    expect(svgColourMatrix({ brightness: 0, contrast: 1, saturation: 1 })).toBe(
      '1 0 0 0 0 0 1 0 0 0 0 0 1 0 0 0 0 0 1 0',
    );
  });
});
