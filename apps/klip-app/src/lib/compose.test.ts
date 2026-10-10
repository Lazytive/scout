/// <reference types="node" />
import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import { compose, layerOutputMs, layerSourceMs, visibleAt } from './compose';
import type { SourceInfo } from './editor';
import { asProject } from './timeline';

const dir = new URL('../../../../crates/klip-edit/fixtures/advanced/', import.meta.url);
const fx = JSON.parse(readFileSync(new URL('compose.json', dir), 'utf8')) as {
  cases: { name: string; source: SourceInfo; clips: [string, SourceInfo][]; project: unknown; expect: Record<string, unknown> }[];
};

/** JSON round trip: what both sides would write. */
const plain = (v: unknown) => JSON.parse(JSON.stringify(v)) as unknown;

describe('compose fixture (crates/klip-edit/fixtures/advanced/compose.json)', () => {
  for (const c of fx.cases) {
    it(c.name, () => {
      const got = compose(asProject(c.project), c.source, c.clips);
      const want = c.expect;
      expect(got.duration_ms).toBe(want.duration_ms);
      expect(plain(got.layers)).toEqual(want.layers);
      expect(plain(got.markers)).toEqual(want.markers);
      expect(got.crop).toEqual(want.crop);
      expect(plain(got.texts)).toEqual(want.texts);
      expect(got.main_volumes).toEqual(want.main_volumes);
      for (const k of ['brightness', 'contrast', 'saturation'] as const) {
        expect(got.colour[k]).toBeCloseTo((want.colour as Record<string, number>)[k]!, 6);
      }
      expect(plain(got.ducking)).toEqual(want.ducking);
    });
  }

  it('maps source and output time both ways on a ramped layer', () => {
    const c = fx.cases[1]!;
    const comp = compose(asProject(c.project), c.source, c.clips);
    const ramped = comp.layers.find((l) => l.speed.length > 1)!;
    for (let t = ramped.at_ms; t < ramped.end_ms; t += 137) {
      const src = layerSourceMs(ramped, t)!;
      const back = layerOutputMs(ramped, src)!;
      expect(layerSourceMs(ramped, back)).toBe(src);
      expect(back).toBeLessThanOrEqual(t);
    }
    // During the crossfade both clips of the montage track are on screen, bottom first.
    const ids = visibleAt(comp, 15_500).map((l) => l.id);
    expect(ids).toEqual([2, 3]);
  });
});
