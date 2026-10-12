import { describe, expect, it } from 'vitest';

import { FROM_BELOW, addClip, overlapOf, readAdvanced, setFromBelow, setTransition, setTransitionAt } from './advanced';
import type { Project } from './timeline';
import { newProject } from './timeline';
import {
  DRAG_PX,
  clipZone,
  fitPps,
  fromHandles,
  joins,
  pastThreshold,
  rowsOf,
  spanMs,
  thumb,
  thumbSlots,
  visibleSpan,
  transitionBoxes,
  transitionLength,
  zoomAt,
} from './timeline-view';

const lib = (key: string) => ({ kind: 'library' as const, key });

function project(): Project {
  return newProject(10_000);
}

describe('drag threshold (a click never moves a clip)', () => {
  it('starts a drag only once the pointer went DRAG_PX away', () => {
    expect(pastThreshold(0, 0)).toBe(false);
    expect(pastThreshold(DRAG_PX - 1, 0)).toBe(false);
    expect(pastThreshold(2, 2)).toBe(false);
    expect(pastThreshold(DRAG_PX, 0)).toBe(true);
    expect(pastThreshold(0, -DRAG_PX)).toBe(true);
  });
});

describe('clip hit zones', () => {
  it('trims at the edges and moves in the middle', () => {
    expect(clipZone(1, 200)).toBe('start');
    expect(clipZone(199, 200)).toBe('end');
    expect(clipZone(100, 200)).toBe('move');
  });
  it('keeps a middle on a narrow clip', () => {
    expect(clipZone(6, 12)).toBe('move');
    expect(clipZone(0, 12)).toBe('start');
    expect(clipZone(12, 12)).toBe('end');
  });
});

describe('rows', () => {
  it('lists video tracks top of the picture first, Main as V1, then Main sound A1 and the sound tracks', () => {
    const p = project();
    addClip(p, 'video', lib('a'), 0, 1000, 10_000);
    addClip(p, 'video', lib('b'), 0, 1000, 10_500); // overlaps a → a second video track
    addClip(p, 'music', { kind: 'file', path: 'm.mp3' }, 0, 5000, 0);
    const rows = rowsOf(readAdvanced(p));
    expect(rows.map((r) => r.label)).toEqual(['V3', 'V2', 'V1', 'A1', 'A2']);
    expect(rows.map((r) => r.kind)).toEqual(['track', 'track', 'main', 'main-sound', 'track']);
    expect(rows.map((r) => r.group)).toEqual(['video', 'video', 'video', 'sound', 'sound']);
  });
});

describe('zoom bar', () => {
  it('spans the edit plus a tail', () => {
    expect(spanMs(10_000)).toBe(12_000);
    expect(spanMs(100_000)).toBe(105_000);
  });
  it('fits the span into the view', () => {
    expect(fitPps(12_000, 600)).toBe(50);
  });
  it('gives the thumb as fractions of the content', () => {
    expect(thumb(0, 500, 1000)).toEqual({ start: 0, end: 0.5 });
    expect(thumb(500, 500, 1000)).toEqual({ start: 0.5, end: 1 });
    expect(thumb(0, 1000, 500)).toEqual({ start: 0, end: 1 });
  });
  it('zooms keeping the time under the pointer in place', () => {
    // 100 px/s, scrolled 200 px, pointer at 100 px: 3 s under it.
    const left = zoomAt(100, 200, 100, 200);
    expect((left + 100) / 200).toBe(3);
    expect(zoomAt(100, 50, 0, 0)).toBe(0);
  });
  it('turns dragged handles into a zoom and a scroll', () => {
    // Showing the middle half of a 12 s span in 600 px: 6 s → 100 px/s, starting at 3 s.
    const v = fromHandles(0.25, 0.75, 12_000, 600, 1000);
    expect(v.pps).toBe(100);
    expect(v.scrollLeft).toBe(300);
  });
  it('never zooms in past the limit nor out past fit', () => {
    expect(fromHandles(0.5, 0.5, 12_000, 600, 1000).pps).toBe(1000);
    expect(fromHandles(-1, 2, 12_000, 600, 1000).pps).toBe(50);
  });
});

describe('thumbnails', () => {
  it('fill the visible part of a long clip back to back', () => {
    // A 10 000 px clip, 44 px rows: 46 px thumbnails; view 2 000…2 400 px of it.
    const v = visibleSpan(1000, 10_000, 3000, 400, 0);
    expect(v).toEqual({ a: 2000, b: 2400 });
    const s = thumbSlots(10_000, 44, 0, 100_000, 100, 100_000, v.a, v.b);
    expect(s.length).toBe(10); // 1 978, 2 024, … 2 392
    expect(s[1]!.x - s[0]!.x).toBe(46);
    expect(s[0]!.x).toBeLessThanOrEqual(2000);
    // The middle of the first one is at 1 978 + 23 px of 10 000: 20 % into the source.
    expect(s[0]!.tile).toBe(20);
  });
  it('shows none in rows too low for a picture', () => {
    expect(thumbSlots(500, 30, 0, 1000, 10, 1000, 0, 500)).toEqual([]);
  });
});

describe('transitions on the timeline', () => {
  function twoClips() {
    const p = project();
    const a = addClip(p, 'video', lib('a'), 0, 4000, 10_000);
    const b = addClip(p, 'video', lib('b'), 0, 4000, 14_000);
    return { p, a, b };
  }

  it('draws a crossfade where the next clip is pulled under the one before', () => {
    const { p, a, b } = twoClips();
    setTransition(p, a, 'crossfade', 500);
    const [box] = transitionBoxes(readAdvanced(p));
    expect(box).toMatchObject({ after: a, next: b, kind: 'crossfade', ms: 500, start: 13_500, end: 14_000 });
  });

  it('centres a dip on the cut', () => {
    const { p, a } = twoClips();
    setTransition(p, a, 'dip', 400);
    const [box] = transitionBoxes(readAdvanced(p));
    expect(box).toMatchObject({ kind: 'dip', start: 13_600, end: 14_400 });
  });

  it('caps the length at the next clip like compose does', () => {
    const p = project();
    const a = addClip(p, 'video', lib('a'), 0, 4000, 10_000);
    addClip(p, 'video', lib('b'), 0, 300, 14_000);
    setTransition(p, a, 'crossfade', 1000);
    const [box] = transitionBoxes(readAdvanced(p));
    expect(box!.end - box!.start).toBe(300);
  });

  it('changes the length by dragging an end', () => {
    expect(transitionLength(500, 'start', -200, 4000)).toBe(700);
    expect(transitionLength(500, 'end', 200, 4000)).toBe(700);
    expect(transitionLength(500, 'start', 1000, 4000)).toBe(100);
    expect(transitionLength(500, 'end', 9000, 4000)).toBe(4000);
  });

  it('draws a crossfade from below over the start of a track first clip', () => {
    const { p, a, b } = twoClips();
    setFromBelow(p, a, 600);
    const adv = readAdvanced(p);
    expect(overlapOf(adv, adv.tracks[0]!, adv.tracks[0]!.clips[0]!)).toBe(600);
    expect(transitionBoxes(adv)).toEqual([
      expect.objectContaining({ after: FROM_BELOW, next: a, kind: 'crossfade', start: 9_400, end: 10_000 }),
    ]);
    expect(() => setFromBelow(p, b, 600)).toThrow();
    setTransitionAt(p, adv.tracks[0]!.id, FROM_BELOW, 'cut', 0);
    expect(transitionBoxes(readAdvanced(p))).toEqual([]);
  });

  it('offers a + only where two clips touch without a transition', () => {
    const { p, a, b } = twoClips();
    const c = addClip(p, 'video', lib('c'), 0, 1000, 20_000); // a gap before c
    expect(joins(readAdvanced(p), 1).map((j) => [j.after, j.next, j.at])).toEqual([[a, b, 14_000]]);
    setTransition(p, a, 'crossfade', 500);
    expect(joins(readAdvanced(p), 1)).toEqual([]);
    void c;
  });
});
