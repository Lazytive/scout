// Effect Controls (M9.7.2): FilmCraft's / Premiere's stopwatch model over Klip's key
// lanes. A group is "animated" once it has a key past the clip's start (or the user
// turned its stopwatch on this session: `sticky`); then a changed value keys the
// playhead, otherwise it changes the clip's constant. Zoom and position share their keys
// (one transform key holds all three), so they form one group, "motion".
import type { Lane, PlacedClip } from './advanced';
import {
  LANES,
  addKey,
  advancedOf,
  clearLane,
  findClip,
  laneValue,
  rateAt,
  removeKey,
  removeSpeedPoint,
  setLane,
  setSpeedPoint,
} from './advanced';
import type { Project } from './timeline';

export type Group = 'motion' | 'opacity' | 'volume' | 'speed';
/** What a value field edits: a lane, or the speed. */
export type Field = Lane | 'speed';

export function groupOf(f: Field): Group {
  return f === 'zoom' || f === 'x' || f === 'y' ? 'motion' : f;
}

/** The times (clip ms) that hold a key in `group`, sorted. */
export function groupTimes(c: PlacedClip, group: Group): number[] {
  const t =
    group === 'motion'
      ? c.transform.map((k) => k.at_ms)
      : group === 'speed'
        ? c.speed.map((s) => s.at_ms)
        : c[group].map((k) => k.at_ms);
  return [...t].sort((a, b) => a - b);
}

export function isAnimated(c: PlacedClip, group: Group, sticky: boolean): boolean {
  return sticky || groupTimes(c, group).some((t) => t > 0);
}

function clipOf(p: Project, clipId: number): PlacedClip | null {
  return findClip(advancedOf(p), clipId)?.clip ?? null;
}

/** Set a value: keyed at clip time `t` when animated, else the clip's constant. */
export function applyValue(p: Project, clipId: number, f: Field, t: number, v: number, animated: boolean): void {
  if (f === 'speed') {
    const c = clipOf(p, clipId);
    if (!c) return;
    if (!animated) c.speed = [];
    setSpeedPoint(p, clipId, animated ? t : 0, v);
    return;
  }
  setLane(p, clipId, f, animated ? t : 0, v);
}

/** Stopwatch on: key the current value at `t` (the start keeps its value). */
export function startAnimating(p: Project, clipId: number, group: Group, t: number): void {
  const c = clipOf(p, clipId);
  if (!c) return;
  if (group === 'speed') setSpeedPoint(p, clipId, t, rateAt(c, t));
  else addKey(p, clipId, group === 'motion' ? 'zoom' : group, t);
}

/** Stopwatch off: drop the keys, keeping the value at `t` for the whole clip. */
export function stopAnimating(p: Project, clipId: number, group: Group, t: number): void {
  const c = clipOf(p, clipId);
  if (!c) return;
  if (group === 'speed') {
    const r = rateAt(c, t);
    c.speed = [];
    if (r !== 100) setSpeedPoint(p, clipId, 0, r);
    return;
  }
  const lanes: Lane[] = group === 'motion' ? ['zoom', 'x', 'y'] : [group];
  const values = lanes.map((l) => laneValue(c, l, t));
  clearLane(p, clipId, lanes[0]!);
  lanes.forEach((l, i) => {
    const neutral = LANES.find((x) => x.id === l)?.neutral;
    const v = values[i]!;
    // Neutral values need no key, unless another lane of the group has one.
    if (v !== neutral || (group === 'motion' && c.transform.length > 0)) setLane(p, clipId, l, 0, v);
  });
  if (group === 'motion' && c.transform.every((k) => k.value.zoom === 1 && k.value.x === 0 && k.value.y === 0)) c.transform = [];
}

/** ◆: add a key at `t`, or remove the one there. */
export function toggleKey(p: Project, clipId: number, group: Group, t: number): void {
  const c = clipOf(p, clipId);
  if (!c) return;
  const has = groupTimes(c, group).includes(t);
  if (group === 'speed') {
    if (has && t > 0) removeSpeedPoint(p, clipId, t);
    else if (!has) setSpeedPoint(p, clipId, t, rateAt(c, t));
    return;
  }
  const lane: Lane = group === 'motion' ? 'zoom' : group;
  if (has) removeKey(p, clipId, lane, t);
  else addKey(p, clipId, lane, t);
}

/** ↺: back to neutral, no keys. */
export function resetGroup(p: Project, clipId: number, group: Group): void {
  const c = clipOf(p, clipId);
  if (!c) return;
  if (group === 'speed') c.speed = [];
  else clearLane(p, clipId, group === 'motion' ? 'zoom' : group);
}

export function prevKey(times: number[], t: number): number | null {
  const before = times.filter((x) => x < t);
  return before.length ? before[before.length - 1]! : null;
}

export function nextKey(times: number[], t: number): number | null {
  return times.find((x) => x > t) ?? null;
}

/** Where to draw diamonds at `xs` (sorted px) so none sits closer than `gap`, within 0…`maxX`. */
export function spreadKeys(xs: number[], gap: number, maxX: number): number[] {
  const out: number[] = [];
  for (const x of xs) out.push(out.length ? Math.max(x, out[out.length - 1]! + gap) : x);
  for (let i = out.length - 1; i >= 0; i--) {
    const limit = i === out.length - 1 ? maxX : out[i + 1]! - gap;
    out[i] = Math.max(0, Math.min(out[i]!, limit));
  }
  return out;
}
