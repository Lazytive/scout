import { describe, expect, it } from 'vitest';

import {
  addClip,
  addKey,
  addMarker,
  clipEnd,
  findClip,
  hasAdvancedEdits,
  laneValue,
  moveClip,
  readAdvanced,
  removeClip,
  setColour,
  setLane,
  setSpeedPoint,
  setTransition,
  splitClip,
  trimClipEnd,
  trimClipStart,
} from './advanced';
import { compose, layerOpacityAt, layerSourceMs, layerTransformAt } from './compose';
import type { SourceInfo } from './editor';
import type { Project } from './timeline';
import { EditError, newProject } from './timeline';

const SOURCE: SourceInfo = { width: 1920, height: 1080, duration_ms: 10_000, fps: 60, bookmarks_ms: [] };
const lib = (key: string) => ({ kind: 'library' as const, key });

function project(): Project {
  return newProject(10_000);
}

describe('placing clips', () => {
  it('puts a clip on the first track of its kind with room, else on a new one', () => {
    const p = project();
    const a = addClip(p, 'video', lib('a'), 0, 4000, 10_000);
    const b = addClip(p, 'video', lib('b'), 0, 3000, 14_000);
    const c = addClip(p, 'video', lib('c'), 0, 3000, 11_000);
    const adv = readAdvanced(p);
    expect(adv.tracks).toHaveLength(2);
    expect(findClip(adv, a)!.track.id).toBe(findClip(adv, b)!.track.id);
    expect(findClip(adv, c)!.track.id).not.toBe(findClip(adv, a)!.track.id);
    expect(() => addClip(p, 'music', { kind: 'image', path: 'x.png' }, 0, 10, 0)).toThrow(EditError);
    expect(hasAdvancedEdits(p)).toBe(true);
    expect(hasAdvancedEdits(project())).toBe(false);
  });

  it('refuses a move onto another clip, drops a track left empty', () => {
    const p = project();
    const a = addClip(p, 'video', lib('a'), 0, 4000, 0);
    const b = addClip(p, 'video', lib('b'), 0, 4000, 5000);
    expect(() => moveClip(p, b, 2000)).toThrow(EditError);
    moveClip(p, b, 4000);
    expect(findClip(readAdvanced(p), b)!.clip.at_ms).toBe(4000);
    moveClip(p, a, 0, 'new');
    expect(readAdvanced(p).tracks).toHaveLength(2);
    moveClip(p, a, 9000, findClip(readAdvanced(p), b)!.track.id);
    expect(readAdvanced(p).tracks).toHaveLength(1);
    removeClip(p, a);
    removeClip(p, b);
    expect(readAdvanced(p).tracks).toHaveLength(0);
  });

  it('trims either edge without moving the picture in time', () => {
    const p = project();
    const id = addClip(p, 'video', lib('a'), 1000, 5000, 2000);
    trimClipStart(p, id, 2500);
    let c = findClip(readAdvanced(p), id)!.clip;
    expect([c.in_ms, c.at_ms, clipEnd(c)]).toEqual([1500, 2500, 6000]);
    trimClipStart(p, id, 0);
    c = findClip(readAdvanced(p), id)!.clip;
    expect([c.in_ms, c.at_ms]).toEqual([0, 1000]);
    trimClipEnd(p, id, 99_000, 5500);
    c = findClip(readAdvanced(p), id)!.clip;
    expect(c.out_ms).toBe(5500);
    trimClipEnd(p, id, 3000, 5500);
    expect(clipEnd(findClip(readAdvanced(p), id)!.clip)).toBe(3000);
  });

  it('splits a clip so the composition looks the same', () => {
    const p = project();
    const id = addClip(p, 'video', lib('a'), 1000, 7000, 500);
    setLane(p, id, 'zoom', 0, 1);
    setLane(p, id, 'zoom', 4000, 2);
    setLane(p, id, 'opacity', 0, 0.2);
    setLane(p, id, 'opacity', 1000, 1);
    setSpeedPoint(p, id, 3000, 50);
    const info: [string, SourceInfo][] = [['a', { ...SOURCE, duration_ms: 9000 }]];
    const before = compose(p, SOURCE, info);
    const second = splitClip(p, id, 2700)!;
    expect(second).toBeGreaterThan(id);
    const after = compose(p, SOURCE, info);
    expect(after.duration_ms).toBe(before.duration_ms);
    const top = (c: typeof before, t: number) => c.layers.filter((l) => l.track !== 0).find((l) => t >= l.at_ms && t < l.end_ms)!;
    for (let t = 500; t < before.duration_ms; t += 50) {
      const a = top(before, t);
      const b = top(after, t);
      if (!a) continue;
      expect(Math.abs(layerSourceMs(b, t)! - layerSourceMs(a, t)!), `t=${t}`).toBeLessThanOrEqual(1);
      expect(layerTransformAt(b, t).zoom).toBeCloseTo(layerTransformAt(a, t).zoom, 3);
      expect(layerOpacityAt(b, t)).toBeCloseTo(layerOpacityAt(a, t), 3);
    }
    expect(() => splitClip(p, id, 0)).toThrow(EditError);
  });
});

describe('keys, speed, transitions', () => {
  it('a value without keys is the clip constant; with keys it adds one at the time', () => {
    const p = project();
    const id = addClip(p, 'video', lib('a'), 0, 4000, 0);
    setLane(p, id, 'x', 1000, 0.5);
    let c = findClip(readAdvanced(p), id)!.clip;
    expect(c.transform).toEqual([{ at_ms: 0, value: { zoom: 1, x: 0.5, y: 0 }, ease: 'linear' }]);
    addKey(p, id, 'zoom', 2000);
    setLane(p, id, 'zoom', 2000, 3);
    c = findClip(readAdvanced(p), id)!.clip;
    expect(c.transform.map((k) => k.at_ms)).toEqual([0, 2000]);
    expect(laneValue(c, 'zoom', 1000)).toBeCloseTo(2, 5);
    expect(laneValue(c, 'x', 3000)).toBeCloseTo(0.5, 5);
    setLane(p, id, 'zoom', 2000, 99);
    expect(laneValue(findClip(readAdvanced(p), id)!.clip, 'zoom', 2000)).toBe(8);
    addKey(p, id, 'volume', 1500);
    c = findClip(readAdvanced(p), id)!.clip;
    expect(c.volume.map((k) => [k.at_ms, k.value])).toEqual([
      [0, 1],
      [1500, 1],
    ]);
  });

  it('speed points change the clip length and vanish at 100 %', () => {
    const p = project();
    const id = addClip(p, 'video', lib('a'), 0, 4000, 0);
    setSpeedPoint(p, id, 0, 200);
    expect(clipEnd(findClip(readAdvanced(p), id)!.clip)).toBe(2000);
    setSpeedPoint(p, id, 1000, 25);
    expect(findClip(readAdvanced(p), id)!.clip.speed).toEqual([
      { at_ms: 0, rate_pct: 200 },
      { at_ms: 1000, rate_pct: 25 },
    ]);
    setSpeedPoint(p, id, 0, 100);
    setSpeedPoint(p, id, 1000, 100);
    expect(findClip(readAdvanced(p), id)!.clip.speed).toEqual([]);
  });

  it('a transition needs a next clip; cut removes it; colour and markers count as edits', () => {
    const p = project();
    const a = addClip(p, 'video', lib('a'), 0, 4000, 0);
    expect(() => setTransition(p, a, 'crossfade', 500)).toThrow(EditError);
    addClip(p, 'video', lib('b'), 0, 4000, 4000);
    setTransition(p, a, 'crossfade', 500);
    expect(readAdvanced(p).transitions).toEqual([{ track: 1, after_clip: a, kind: 'crossfade', ms: 500 }]);
    setTransition(p, a, 'cut', 0);
    expect(readAdvanced(p).transitions).toEqual([]);
    const q = project();
    setColour(q, { saturation: 3 });
    expect(readAdvanced(q).colour.saturation).toBe(2);
    expect(hasAdvancedEdits(q)).toBe(true);
    const r = project();
    addMarker(r, 1234, '  Drop\u0007 ');
    expect(readAdvanced(r).markers).toEqual([{ at_ms: 1234, title: 'Drop', kind: 'user' }]);
  });
});
