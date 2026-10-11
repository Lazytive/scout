import { describe, expect, it } from 'vitest';

import { addClip, findClip, laneValue, rateAt, readAdvanced, setLane } from './advanced';
import type { PlacedClip } from './advanced';
import {
  applyValue,
  groupTimes,
  isAnimated,
  nextKey,
  prevKey,
  resetGroup,
  spreadKeys,
  startAnimating,
  stopAnimating,
  toggleKey,
} from './effect-controls';
import type { Project } from './timeline';
import { newProject } from './timeline';

const lib = (key: string) => ({ kind: 'library' as const, key });

function setup(): { p: Project; id: number; clip: () => PlacedClip } {
  const p = newProject(10_000);
  const id = addClip(p, 'video', lib('a'), 0, 4000, 10_000);
  return { p, id, clip: () => findClip(readAdvanced(p), id)!.clip };
}

describe('the stopwatch', () => {
  it('is off for a constant and on once a key sits past the start', () => {
    const { p, id, clip } = setup();
    expect(isAnimated(clip(), 'motion', false)).toBe(false);
    setLane(p, id, 'zoom', 0, 1.5);
    expect(isAnimated(clip(), 'motion', false)).toBe(false);
    setLane(p, id, 'zoom', 1000, 2);
    expect(isAnimated(clip(), 'motion', false)).toBe(true);
    expect(isAnimated(clip(), 'opacity', true)).toBe(true);
  });

  it('turning it on keys the current value at the playhead and keeps the start', () => {
    const { p, id, clip } = setup();
    setLane(p, id, 'zoom', 0, 1.5);
    startAnimating(p, id, 'motion', 2000);
    expect(groupTimes(clip(), 'motion')).toEqual([0, 2000]);
    expect(laneValue(clip(), 'zoom', 2000)).toBe(1.5);
  });

  it('turning it off keeps the value at the playhead for the whole clip', () => {
    const { p, id, clip } = setup();
    setLane(p, id, 'opacity', 0, 1);
    setLane(p, id, 'opacity', 2000, 0.5);
    stopAnimating(p, id, 'opacity', 1000);
    expect(groupTimes(clip(), 'opacity')).toEqual([0]);
    expect(laneValue(clip(), 'opacity', 3000)).toBeCloseTo(0.75);
  });

  it('turning motion off keeps zoom and position together', () => {
    const { p, id, clip } = setup();
    setLane(p, id, 'zoom', 0, 1);
    setLane(p, id, 'zoom', 2000, 2);
    setLane(p, id, 'x', 2000, 0.4);
    stopAnimating(p, id, 'motion', 2000);
    expect(clip().transform).toEqual([{ at_ms: 0, value: { zoom: 2, x: 0.4, y: 0 }, ease: 'linear' }]);
  });

  it('speed: on adds a point at the playhead, off keeps the speed there', () => {
    const { p, id, clip } = setup();
    applyValue(p, id, 'speed', 0, 50, false);
    expect(clip().speed).toEqual([{ at_ms: 0, rate_pct: 50 }]);
    startAnimating(p, id, 'speed', 1000);
    applyValue(p, id, 'speed', 1000, 200, true);
    expect(rateAt(clip(), 1500)).toBe(200);
    stopAnimating(p, id, 'speed', 1500);
    expect(clip().speed).toEqual([{ at_ms: 0, rate_pct: 200 }]);
  });
});

describe('changing a value', () => {
  it('sets the constant while the stopwatch is off, wherever the playhead is', () => {
    const { p, id, clip } = setup();
    applyValue(p, id, 'zoom', 3000, 1.5, false);
    applyValue(p, id, 'zoom', 1000, 1.8, false);
    expect(groupTimes(clip(), 'motion')).toEqual([0]);
    expect(laneValue(clip(), 'zoom', 3000)).toBe(1.8);
  });

  it('keys it at the playhead while the stopwatch is on', () => {
    const { p, id, clip } = setup();
    applyValue(p, id, 'volume', 0, 1, false);
    applyValue(p, id, 'volume', 2000, 0.5, true);
    expect(groupTimes(clip(), 'volume')).toEqual([0, 2000]);
  });
});

describe('the key navigator', () => {
  it('adds and removes a key at the playhead', () => {
    const { p, id, clip } = setup();
    toggleKey(p, id, 'opacity', 1000);
    expect(groupTimes(clip(), 'opacity')).toEqual([0, 1000]);
    toggleKey(p, id, 'opacity', 1000);
    expect(groupTimes(clip(), 'opacity')).toEqual([0]);
  });

  it('finds the key before and after the playhead', () => {
    const times = [0, 1000, 2500];
    expect(prevKey(times, 1000)).toBe(0);
    expect(prevKey(times, 1200)).toBe(1000);
    expect(prevKey(times, 0)).toBeNull();
    expect(nextKey(times, 1000)).toBe(2500);
    expect(nextKey(times, 2500)).toBeNull();
  });

  it('resets a group to neutral', () => {
    const { p, id, clip } = setup();
    setLane(p, id, 'zoom', 0, 2);
    setLane(p, id, 'zoom', 1000, 3);
    resetGroup(p, id, 'motion');
    expect(clip().transform).toEqual([]);
  });
});

describe('crowded diamonds', () => {
  it('spreads keys closer than the gap, keeping the order and the lane', () => {
    expect(spreadKeys([0, 2, 3, 50], 10, 100)).toEqual([0, 10, 20, 50]);
    expect(spreadKeys([95, 96, 97], 10, 100)).toEqual([80, 90, 100]);
    expect(spreadKeys([], 10, 100)).toEqual([]);
  });
});
