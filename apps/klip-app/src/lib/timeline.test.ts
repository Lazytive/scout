/// <reference types="node" />
import { readFileSync, readdirSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import {
  EditError,
  type Project,
  type Speed,
  asProject,
  cloneProject,
  cut,
  newProject,
  outputDurationMs,
  outputTime,
  segments,
  setSpeed,
  sourceTime,
  split,
  trim,
} from './timeline';

interface Fixture {
  name: string;
  project: Record<string, unknown>;
  expect: {
    output_duration_ms: number;
    segments: unknown[];
    source_time: [number, number | null][];
    output_time: [number, number | null][];
  };
}

const dir = new URL('../../../../crates/klip-edit/fixtures/', import.meta.url);
const files = readdirSync(dir)
  .filter((f) => f.endsWith('.json'))
  .sort();

describe('shared fixtures (crates/klip-edit/fixtures)', () => {
  it('has the fixtures the Rust side checks', () => {
    expect(files.length).toBeGreaterThanOrEqual(3);
  });

  describe.each(files)('%s', (file) => {
    const fx = JSON.parse(readFileSync(new URL(file, dir), 'utf8')) as Fixture;
    const p = asProject(fx.project);

    it('has the same output length', () => {
      expect(outputDurationMs(p)).toBe(fx.expect.output_duration_ms);
    });

    it('places the same segments', () => {
      expect(segments(p)).toEqual(fx.expect.segments);
    });

    it('maps output time to source time like the export', () => {
      for (const [out, src] of fx.expect.source_time) {
        expect(sourceTime(p, out), `out ${out}`).toBe(src);
      }
    });

    it('maps source time to output time like the preview', () => {
      for (const [src, out] of fx.expect.output_time) {
        expect(outputTime(p, src), `src ${src}`).toBe(out);
      }
    });
  });
});

function project(sourceMs: number, pieces: [number, number, Speed][] = []): Project {
  const p = newProject(sourceMs);
  p.pieces = pieces.map(([start_ms, end_ms, speed]) => ({ start_ms, end_ms, speed }));
  return p;
}

describe('edits', () => {
  it('a project without pieces is the whole clip', () => {
    const p = project(10_000);
    expect(segments(p)).toEqual([
      { src_start_ms: 0, src_end_ms: 10_000, speed: 1, out_start_ms: 0, out_end_ms: 10_000 },
    ]);
    expect(sourceTime(p, 9_999)).toBe(9_999);
    expect(sourceTime(p, 10_000)).toBeNull();
    expect(segments(project(0))).toEqual([]);
  });

  it('split makes two pieces with the same picture', () => {
    const p = project(10_000);
    expect(split(p, 4_000)).toBe(true);
    expect(p.pieces).toEqual([
      { start_ms: 0, end_ms: 4_000, speed: 1 },
      { start_ms: 4_000, end_ms: 10_000, speed: 1 },
    ]);
    expect(split(p, 4_000)).toBe(false);
    expect(split(p, 0)).toBe(false);
    expect(split(p, 10_000)).toBe(false);
    expect(() => split(p, 10_001)).toThrow(EditError);
    expect(() => split(project(0), 1)).toThrow(/length/);
  });

  it('cut removes the middle and shifts the output', () => {
    const p = project(10_000);
    cut(p, 2_000, 5_000);
    expect(segments(p)).toEqual([
      { src_start_ms: 0, src_end_ms: 2_000, speed: 1, out_start_ms: 0, out_end_ms: 2_000 },
      { src_start_ms: 5_000, src_end_ms: 10_000, speed: 1, out_start_ms: 2_000, out_end_ms: 7_000 },
    ]);
    expect(sourceTime(p, 2_000)).toBe(5_000);
    expect(outputTime(p, 3_000)).toBeNull();
    const before = cloneProject(p);
    expect(() => cut(p, 0, 7_000)).toThrow(/whole clip/);
    expect(p).toEqual(before);
    expect(() => cut(p, 5, 5)).toThrow(EditError);
  });

  it('trim keeps the range', () => {
    const p = project(10_000);
    trim(p, 1_000, 3_000);
    expect(p.pieces).toEqual([{ start_ms: 1_000, end_ms: 3_000, speed: 1 }]);
    expect(outputDurationMs(p)).toBe(2_000);
  });

  it('set_speed splits and changes only the range', () => {
    const p = project(10_000);
    setSpeed(p, 2_000, 6_000, 2);
    expect(p.pieces).toEqual([
      { start_ms: 0, end_ms: 2_000, speed: 1 },
      { start_ms: 2_000, end_ms: 6_000, speed: 2 },
      { start_ms: 6_000, end_ms: 10_000, speed: 1 },
    ]);
    expect(outputDurationMs(p)).toBe(8_000);
    expect(sourceTime(p, 2_500)).toBe(3_000);
    setSpeed(p, 0, 2_000, 0.5);
    expect(outputDurationMs(p)).toBe(10_000);
    expect(sourceTime(p, 1)).toBe(0);
    expect(sourceTime(p, 2)).toBe(1);
  });

  it('half speed over an odd length rounds up', () => {
    const p = project(10_000, [[2_000, 3_001, 0.5]]);
    expect(outputDurationMs(p)).toBe(2_002);
  });

  it('asProject keeps unknown keys and fills missing ones', () => {
    const p = asProject({ source_ms: 5, bogus: { advanced: true } });
    expect(p.source_ms).toBe(5);
    expect(p.format).toEqual({ kind: 'wide', crop_x: 0.5 });
    expect(p.bogus).toEqual({ advanced: true });
    expect(asProject(null).pieces).toEqual([]);
    expect(asProject({ pieces: 'no', text: 3 }).text).toEqual([]);
  });
});
