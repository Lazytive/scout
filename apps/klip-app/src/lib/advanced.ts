/**
 * Advanced mode's part of the project (`klip_edit::advanced`, ADR 0018): the types and
 * the edits the workspace makes (place, move, trim, split and remove clips; keyframes;
 * speed points; transitions; markers; colour; ducking). Every edit keeps what
 * `Advanced::repair` would enforce (sorted unique keys, values in range, caps), so the
 * preview shows what the engine will save. Pure: `editor.svelte.ts` runs these on a
 * copy of the project. Unit-tested in `advanced.test.ts`.
 */

import type { Colour, Ease, Keyframe, SpeedPoint, Transform, TransformKey } from './keyframes';
import { IDENTITY_COLOUR, outputLength, sourceOffset, transformAt, valueAt } from './keyframes';
import type { Project } from './timeline';
import { EditError, MAX_VOLUME } from './timeline';

export const MAX_TRACKS = 16;
export const MAX_CLIPS = 256;
export const MAX_KEYFRAMES = 512;
export const MAX_SPEED_POINTS = 64;
export const MAX_TRANSITIONS = 256;
export const MAX_MARKERS = 1024;
export const MAX_TITLE_CHARS = 120;
export const RATE_MIN = 25;
export const RATE_MAX = 400;
export const ZOOM_MIN = 0.1;
export const ZOOM_MAX = 8;
export const MAX_TRANSITION_MS = 10_000;
export const MAX_SOURCE_MS = 24 * 60 * 60 * 1000;
/** How long a new picture (sticker) stays on screen. */
export const STICKER_MS = 3_000;

export type Source =
  | { kind: 'main' }
  | { kind: 'library'; key: string }
  | { kind: 'file'; path: string }
  | { kind: 'image'; path: string };

export type TrackKind = 'video' | 'text' | 'audio' | 'music';

export interface PlacedClip {
  id: number;
  source: Source;
  in_ms: number;
  out_ms: number;
  at_ms: number;
  transform: TransformKey[];
  opacity: Keyframe[];
  volume: Keyframe[];
  speed: SpeedPoint[];
}

export interface Track {
  id: number;
  kind: TrackKind;
  name: string;
  muted: boolean;
  clips: PlacedClip[];
}

export type TransitionKind = 'cut' | 'crossfade' | 'dip';

export interface Transition {
  track: number;
  after_clip: number;
  kind: TransitionKind;
  ms: number;
}

export type MarkerKind = 'bookmark' | 'event' | 'user';

export interface Marker {
  at_ms: number;
  title: string;
  kind: MarkerKind;
}

export interface Ducking {
  enabled: boolean;
  duck_db: number;
  attack_ms: number;
  release_ms: number;
}

export interface Advanced {
  tracks: Track[];
  transitions: Transition[];
  colour: Colour;
  markers: Marker[];
  ducking: Ducking;
}

export const DEFAULT_DUCKING: Ducking = { enabled: true, duck_db: -12, attack_ms: 100, release_ms: 600 };

export function emptyAdvanced(): Advanced {
  return { tracks: [], transitions: [], colour: { ...IDENTITY_COLOUR }, markers: [], ducking: { ...DEFAULT_DUCKING } };
}

function isObj(v: unknown): v is Record<string, unknown> {
  return !!v && typeof v === 'object' && !Array.isArray(v);
}

/** The project's advanced section as this build reads it (defaults for what is missing). */
export function readAdvanced(p: Project): Advanced {
  const raw = p.advanced;
  const a = emptyAdvanced();
  if (!isObj(raw)) return a;
  if (Array.isArray(raw.tracks)) a.tracks = raw.tracks.filter(isObj) as unknown as Track[];
  for (const t of a.tracks) if (!Array.isArray(t.clips)) t.clips = [];
  for (const t of a.tracks)
    for (const c of t.clips) {
      for (const k of ['transform', 'opacity', 'volume', 'speed'] as const) if (!Array.isArray(c[k])) c[k] = [];
      if (!isObj(c.source)) c.source = { kind: 'main' };
    }
  if (Array.isArray(raw.transitions)) a.transitions = raw.transitions.filter(isObj) as unknown as Transition[];
  if (isObj(raw.colour)) a.colour = { ...a.colour, ...(raw.colour as Partial<Colour>) };
  if (Array.isArray(raw.markers)) a.markers = raw.markers.filter(isObj) as unknown as Marker[];
  if (isObj(raw.ducking)) a.ducking = { ...a.ducking, ...(raw.ducking as Partial<Ducking>) };
  return a;
}

/** The section on `p`, created when missing (edits mutate it in place). */
export function advancedOf(p: Project): Advanced {
  const a = readAdvanced(p);
  p.advanced = a;
  return a;
}

/** `Advanced::is_empty`: nothing an Advanced-mode user did. */
export function isAdvancedEmpty(a: Advanced): boolean {
  const c = a.colour;
  return (
    a.tracks.every((t) => t.clips.length === 0) &&
    a.transitions.length === 0 &&
    c.brightness === 0 &&
    c.contrast === 1 &&
    c.saturation === 1 &&
    a.markers.every((m) => m.kind !== 'user')
  );
}

const KNOWN_KEYS = new Set(['version', 'mode', 'source_ms', 'pieces', 'format', 'text', 'volumes', 'advanced']);

/** `Project::has_advanced_edits`: what the "advanced edits" badge in Normal mode shows. */
export function hasAdvancedEdits(p: Project): boolean {
  if (!isAdvancedEmpty(readAdvanced(p))) return true;
  return Object.keys(p).some((k) => !KNOWN_KEYS.has(k));
}

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, Number.isFinite(v) ? v : lo));
}

function ms(v: number): number {
  return Math.max(0, Math.min(MAX_SOURCE_MS, Math.round(Number.isFinite(v) ? v : 0)));
}

/** A track name or marker title as `cut_text` keeps it. */
export function cleanTitle(s: string): string {
  return Array.from(s)
    .filter((c) => !/\p{Cc}/u.test(c))
    .slice(0, MAX_TITLE_CHARS)
    .join('')
    .trim();
}

/** Sort by time, one key per time (the last wins), capped. */
function tidy<T extends { at_ms: number }>(keys: T[], cap: number): T[] {
  const sorted = keys.map((k, i) => [k, i] as const).sort((a, b) => a[0].at_ms - b[0].at_ms || a[1] - b[1]);
  const out: T[] = [];
  for (const [k] of sorted) {
    if (out.length && out[out.length - 1]!.at_ms === k.at_ms) out.pop();
    out.push(k);
  }
  return out.slice(0, cap);
}

export function nextTrackId(a: Advanced): number {
  return a.tracks.reduce((m, t) => Math.max(m, t.id), 0) + 1;
}

export function nextClipId(a: Advanced): number {
  return a.tracks.reduce((m, t) => t.clips.reduce((n, c) => Math.max(n, c.id), m), 0) + 1;
}

export interface Found {
  track: Track;
  clip: PlacedClip;
  index: number;
}

export function findClip(a: Advanced, clipId: number): Found | null {
  for (const track of a.tracks) {
    const index = track.clips.findIndex((c) => c.id === clipId);
    if (index >= 0) return { track, clip: track.clips[index]!, index };
  }
  return null;
}

/** A clip's length on the timeline (after its speed ramp), ms. */
export function clipLength(c: PlacedClip): number {
  return outputLength(c.speed, Math.max(0, c.out_ms - c.in_ms));
}

export function clipEnd(c: PlacedClip): number {
  return c.at_ms + clipLength(c);
}

/**
 * How far a clip is pulled under the one before it by a crossfade (ms; 0 without):
 * where `compose` really places it is `at_ms` minus this.
 */
export function overlapOf(a: Advanced, track: Track, clip: PlacedClip): number {
  const i = track.clips.findIndex((c) => c.id === clip.id);
  const prev = i > 0 ? track.clips[i - 1] : undefined;
  if (!prev) return 0;
  const tr = a.transitions.find((t) => t.track === track.id && t.after_clip === prev.id);
  if (!tr || tr.kind !== 'crossfade') return 0;
  return Math.min(tr.ms, Math.max(0, clip.out_ms - clip.in_ms), clip.at_ms);
}

/** Which track kinds a source may sit on. */
export function fitsTrack(source: Source, kind: TrackKind): boolean {
  if (source.kind === 'image') return kind === 'video';
  if (source.kind === 'file') return kind === 'audio' || kind === 'music';
  return kind === 'video';
}

function overlaps(track: Track, at: number, end: number, except: number | null): boolean {
  return track.clips.some((c) => c.id !== except && at < clipEnd(c) && end > c.at_ms);
}

function sortClips(track: Track): void {
  track.clips.sort((x, y) => x.at_ms - y.at_ms || x.id - y.id);
}

const TRACK_NAMES: Record<TrackKind, string> = { video: 'Video', text: 'Text', audio: 'Sound', music: 'Music' };

function newTrack(a: Advanced, kind: TrackKind, base = TRACK_NAMES[kind]): Track {
  if (a.tracks.length >= MAX_TRACKS) throw new EditError('too_many_pieces', `A project has at most ${MAX_TRACKS} tracks`);
  const n = a.tracks.filter((t) => t.name.startsWith(base)).length + 1;
  const track: Track = { id: nextTrackId(a), kind, name: n === 1 ? base : `${base} ${n}`, muted: false, clips: [] };
  a.tracks.push(track);
  return track;
}

/** Drop tracks left without clips (an empty track isn't saved anyway). */
function dropEmptyTracks(a: Advanced): void {
  const gone = new Set(a.tracks.filter((t) => t.clips.length === 0).map((t) => t.id));
  if (gone.size === 0) return;
  a.tracks = a.tracks.filter((t) => !gone.has(t.id));
  a.transitions = a.transitions.filter((t) => !gone.has(t.track));
}

/**
 * Place `source` (`inMs..outMs` of it) at `atMs`: on `trackId` when given, else on the
 * first track of `kind` with room, else on a new track. Returns the clip's id.
 */
export function addClip(p: Project, kind: TrackKind, source: Source, inMs: number, outMs: number, atMs: number, trackId?: number): number {
  const a = advancedOf(p);
  if (!fitsTrack(source, kind)) throw new EditError('bad_range', 'That doesn’t go on this kind of track');
  const start = ms(inMs);
  const end = ms(outMs);
  if (end <= start) throw new EditError('bad_range', 'The clip is empty');
  const at = ms(atMs);
  const len = end - start;
  let track = trackId === undefined ? undefined : a.tracks.find((t) => t.id === trackId);
  if (track && (track.kind !== kind || overlaps(track, at, at + len, null))) track = undefined;
  // Pictures go on picture tracks (over the video), videos on video tracks.
  const picture = source.kind === 'image';
  const sameSort = (t: Track) => t.clips.length > 0 && t.clips.every((c) => (c.source.kind === 'image') === picture);
  track ??= a.tracks.find((t) => t.kind === kind && sameSort(t) && !overlaps(t, at, at + len, null)) ?? newTrack(a, kind, picture ? 'Stickers' : undefined);
  if (track.clips.length >= MAX_CLIPS) throw new EditError('too_many_pieces', `A track holds at most ${MAX_CLIPS} clips`);
  const id = nextClipId(a);
  track.clips.push({ id, source, in_ms: start, out_ms: end, at_ms: at, transform: [], opacity: [], volume: [], speed: [] });
  sortClips(track);
  return id;
}

/**
 * Move a clip to `atMs` (and to `trackId` when given, a track of a kind it fits, or
 * `'new'` for a new track of its kind). A move onto another clip is refused.
 */
export function moveClip(p: Project, clipId: number, atMs: number, trackId?: number | 'new'): void {
  const a = advancedOf(p);
  const found = findClip(a, clipId);
  if (!found) return;
  const { clip, track } = found;
  const at = ms(atMs);
  const len = clipLength(clip);
  let target = track;
  if (trackId === 'new') target = newTrack(a, track.kind);
  else if (trackId !== undefined && trackId !== track.id) {
    const t = a.tracks.find((x) => x.id === trackId);
    if (!t || !fitsTrack(clip.source, t.kind)) throw new EditError('bad_range', 'That clip doesn’t go on that track');
    target = t;
  }
  if (overlaps(target, at, at + len, clip.id)) throw new EditError('bad_range', 'Clips can’t overlap on one track');
  clip.at_ms = at;
  if (target !== track) {
    track.clips = track.clips.filter((c) => c.id !== clip.id);
    if (target.clips.length >= MAX_CLIPS) throw new EditError('too_many_pieces', `A track holds at most ${MAX_CLIPS} clips`);
    target.clips.push(clip);
    a.transitions = a.transitions.filter((t) => !(t.track === track.id && t.after_clip === clip.id));
  }
  sortClips(target);
  dropEmptyTracks(a);
}

/** Keys of one lane moved by `-shift` ms of clip time, cut to `0..len`, with the values at the cut ends kept. */
function shiftKeys<T extends { at_ms: number }>(keys: T[], shift: number, len: number, at: (t: number) => T | null): T[] {
  if (keys.length === 0) return keys;
  const out: T[] = [];
  const first = at(shift);
  if (first && shift > 0) out.push({ ...first, at_ms: 0 });
  for (const k of keys) {
    const t = k.at_ms - shift;
    if (t >= 0 && t <= len) out.push({ ...k, at_ms: t });
  }
  const last = at(shift + len);
  if (last && keys[keys.length - 1]!.at_ms > shift + len) out.push({ ...last, at_ms: len });
  return tidy(out, MAX_KEYFRAMES);
}

function keyAt(keys: Keyframe[], t: number): Keyframe | null {
  const v = valueAt(keys, t);
  if (v === null) return null;
  const before = [...keys].reverse().find((k) => k.at_ms <= t);
  return { at_ms: t, value: v, ease: before?.ease ?? 'linear' };
}

function transformKeyAt(keys: TransformKey[], t: number): TransformKey | null {
  const v = transformAt(keys, t);
  if (v === null) return null;
  const before = [...keys].reverse().find((k) => k.at_ms <= t);
  return { at_ms: t, value: { ...v }, ease: before?.ease ?? 'linear' };
}

/** Speed points from clip time `shift` on, starting at 0. */
function shiftSpeed(points: SpeedPoint[], shift: number, len: number): SpeedPoint[] {
  if (points.length === 0) return points;
  const rateAt = (t: number) => [...points].reverse().find((p) => p.at_ms <= t)?.rate_pct ?? 100;
  const out: SpeedPoint[] = [{ at_ms: 0, rate_pct: rateAt(shift) }];
  for (const p of points) {
    const t = p.at_ms - shift;
    if (t > 0 && t < len) out.push({ ...p, at_ms: t });
  }
  const tidied = tidy(out, MAX_SPEED_POINTS);
  return tidied.length === 1 && tidied[0]!.rate_pct === 100 ? [] : tidied;
}

function moveLanes(c: PlacedClip, shift: number, len: number): void {
  c.transform = shiftKeys(c.transform, shift, len, (t) => transformKeyAt(c.transform, t));
  c.opacity = shiftKeys(c.opacity, shift, len, (t) => keyAt(c.opacity, t));
  c.volume = shiftKeys(c.volume, shift, len, (t) => keyAt(c.volume, t));
  c.speed = shiftSpeed(c.speed, shift, len);
}

/** Pictures have no length of their own: their source range is just how long they show. */
function isPicture(c: PlacedClip): boolean {
  return c.source.kind === 'image';
}

/**
 * Move a clip's start edge to timeline time `atMs` (the picture stays where it was in
 * time; it can't start before its source does).
 */
export function trimClipStart(p: Project, clipId: number, atMs: number): void {
  const a = advancedOf(p);
  const found = findClip(a, clipId);
  if (!found) return;
  const { clip, track } = found;
  const end = clipEnd(clip);
  let at = Math.min(ms(atMs), end - 1);
  // Not over the previous clip of the track.
  const prev = track.clips.filter((c) => c.id !== clip.id && clipEnd(c) <= clip.at_ms).reduce((m, c) => Math.max(m, clipEnd(c)), 0);
  at = Math.max(at, prev);
  const delta = at - clip.at_ms;
  if (delta === 0) return;
  if (delta > 0) {
    const srcDelta = sourceOffset(clip.speed, delta);
    if (!isPicture(clip)) clip.in_ms = Math.min(clip.out_ms - 1, clip.in_ms + srcDelta);
    else clip.out_ms = Math.max(clip.in_ms + 1, clip.out_ms - delta);
    moveLanes(clip, delta, end - at);
  } else {
    const grow = -delta;
    const rate = (clip.speed[0]?.at_ms === 0 ? clip.speed[0].rate_pct : 100) / 100;
    if (isPicture(clip)) clip.out_ms = Math.min(MAX_SOURCE_MS, clip.out_ms + grow);
    else {
      const room = clip.in_ms;
      const srcGrow = Math.min(room, Math.floor(grow * rate));
      if (srcGrow <= 0) return;
      clip.in_ms -= srcGrow;
      at = clip.at_ms - Math.ceil(srcGrow / rate);
    }
    const g = clip.at_ms - at;
    shiftForward(clip, g);
  }
  clip.at_ms = Math.max(0, at);
  sortClips(track);
}

function shiftForward(c: PlacedClip, g: number): void {
  const fwd = <T extends { at_ms: number }>(keys: T[]) => keys.map((k) => ({ ...k, at_ms: k.at_ms + g }));
  c.transform = fwd(c.transform);
  c.opacity = fwd(c.opacity);
  c.volume = fwd(c.volume);
  if (c.speed.length) {
    const first = c.speed[0]!.at_ms === 0 ? c.speed[0]!.rate_pct : 100;
    c.speed = tidy([{ at_ms: 0, rate_pct: first }, ...fwd(c.speed).filter((s) => s.at_ms > g)], MAX_SPEED_POINTS);
  }
}

/** Move a clip's end edge to timeline time `endMs`; `srcMax` bounds a video/sound. */
export function trimClipEnd(p: Project, clipId: number, endMs: number, srcMax: number): void {
  const a = advancedOf(p);
  const found = findClip(a, clipId);
  if (!found) return;
  const { clip, track } = found;
  const next = track.clips.filter((c) => c.id !== clip.id && c.at_ms >= clipEnd(clip)).reduce((m, c) => Math.min(m, c.at_ms), MAX_SOURCE_MS);
  const end = Math.min(Math.max(ms(endMs), clip.at_ms + 1), next);
  const len = end - clip.at_ms;
  if (isPicture(clip)) clip.out_ms = clip.in_ms + len;
  else {
    const max = srcMax > clip.in_ms ? srcMax : clip.out_ms;
    clip.out_ms = Math.min(max, Math.max(clip.in_ms + 1, clip.in_ms + sourceOffset(clip.speed, len)));
  }
  const newLen = clipLength(clip);
  clip.transform = clip.transform.filter((k) => k.at_ms <= newLen);
  clip.opacity = clip.opacity.filter((k) => k.at_ms <= newLen);
  clip.volume = clip.volume.filter((k) => k.at_ms <= newLen);
  clip.speed = clip.speed.filter((s) => s.at_ms < newLen);
}

/** Split a clip at timeline time `atMs` into two; the second gets a new id. */
export function splitClip(p: Project, clipId: number, atMs: number): number | null {
  const a = advancedOf(p);
  const found = findClip(a, clipId);
  if (!found) return null;
  const { clip, track } = found;
  const local = Math.round(atMs) - clip.at_ms;
  const len = clipLength(clip);
  if (local <= 0 || local >= len) throw new EditError('bad_range', 'Put the playhead inside the clip to split it');
  if (track.clips.length >= MAX_CLIPS) throw new EditError('too_many_pieces', `A track holds at most ${MAX_CLIPS} clips`);
  const second: PlacedClip = JSON.parse(JSON.stringify(clip)) as PlacedClip;
  second.id = nextClipId(a);
  const srcCut = isPicture(clip) ? clip.in_ms + local : Math.min(clip.out_ms - 1, clip.in_ms + sourceOffset(clip.speed, local));
  second.in_ms = srcCut;
  second.at_ms = clip.at_ms + local;
  moveLanes(second, local, len - local);
  clip.out_ms = srcCut;
  if (isPicture(clip)) clip.out_ms = clip.in_ms + local;
  const firstLen = clipLength(clip);
  const keep = <T extends { at_ms: number }>(keys: T[], at: (t: number) => T | null): T[] => {
    if (keys.length === 0) return keys;
    const out = keys.filter((k) => k.at_ms <= firstLen);
    const end = at(firstLen);
    if (end && keys[keys.length - 1]!.at_ms > firstLen) out.push({ ...end, at_ms: firstLen });
    return tidy(out, MAX_KEYFRAMES);
  };
  const t0 = clip.transform;
  const o0 = clip.opacity;
  const v0 = clip.volume;
  clip.transform = keep(t0, (t) => transformKeyAt(t0, t));
  clip.opacity = keep(o0, (t) => keyAt(o0, t));
  clip.volume = keep(v0, (t) => keyAt(v0, t));
  clip.speed = clip.speed.filter((s) => s.at_ms < firstLen);
  // The transition after the clip now follows its second half.
  for (const t of a.transitions) if (t.track === track.id && t.after_clip === clip.id) t.after_clip = second.id;
  track.clips.push(second);
  sortClips(track);
  return second.id;
}

export function removeClip(p: Project, clipId: number): void {
  const a = advancedOf(p);
  const found = findClip(a, clipId);
  if (!found) return;
  const { track, index } = found;
  const prev = track.clips[index - 1];
  track.clips.splice(index, 1);
  a.transitions = a.transitions.filter((t) => !(t.track === track.id && (t.after_clip === clipId || t.after_clip === prev?.id)));
  dropEmptyTracks(a);
}

export type Lane = 'zoom' | 'x' | 'y' | 'opacity' | 'volume';

export const LANES: { id: Lane; label: string; min: number; max: number; step: number; neutral: number }[] = [
  { id: 'zoom', label: 'Zoom', min: ZOOM_MIN, max: ZOOM_MAX, step: 0.05, neutral: 1 },
  { id: 'x', label: 'X', min: -1, max: 1, step: 0.01, neutral: 0 },
  { id: 'y', label: 'Y', min: -1, max: 1, step: 0.01, neutral: 0 },
  { id: 'opacity', label: 'Opacity', min: 0, max: 1, step: 0.01, neutral: 1 },
  { id: 'volume', label: 'Volume', min: 0, max: MAX_VOLUME, step: 0.05, neutral: 1 },
];

function laneSpec(lane: Lane) {
  return LANES.find((l) => l.id === lane)!;
}

/** A lane's value at clip time `t` (the neutral value without keys). */
export function laneValue(c: PlacedClip, lane: Lane, t: number): number {
  if (lane === 'zoom' || lane === 'x' || lane === 'y') {
    const v = transformAt(c.transform, t);
    return v ? v[lane] : laneSpec(lane).neutral;
  }
  return valueAt(c[lane], t) ?? laneSpec(lane).neutral;
}

/** Times (clip ms) that hold a key on `lane`. */
export function laneKeys(c: PlacedClip, lane: Lane): number[] {
  const keys = lane === 'zoom' || lane === 'x' || lane === 'y' ? c.transform : c[lane];
  return keys.map((k) => k.at_ms);
}

/**
 * Set `lane` to `value` at clip time `t`. Without keys on the lane the value becomes
 * the clip's constant (one key at 0); with keys, a key at `t` is added or updated.
 */
export function setLane(p: Project, clipId: number, lane: Lane, t: number, value: number, ease: Ease = 'linear'): void {
  const found = findClip(advancedOf(p), clipId);
  if (!found) return;
  const { clip } = found;
  const spec = laneSpec(lane);
  const v = clamp(value, spec.min, spec.max);
  const at = Math.min(Math.max(0, Math.round(t)), clipLength(clip));
  if (lane === 'zoom' || lane === 'x' || lane === 'y') {
    if (clip.transform.length === 0) {
      const value: Transform = { zoom: 1, x: 0, y: 0, [lane]: v };
      clip.transform = [{ at_ms: 0, value, ease }];
      return;
    }
    const current = transformAt(clip.transform, at) ?? { zoom: 1, x: 0, y: 0 };
    const existing = clip.transform.find((k) => k.at_ms === at);
    if (existing) existing.value = { ...existing.value, [lane]: v };
    else clip.transform = tidy([...clip.transform, { at_ms: at, value: { ...current, [lane]: v }, ease }], MAX_KEYFRAMES);
    return;
  }
  if (clip[lane].length === 0) {
    clip[lane] = [{ at_ms: 0, value: v, ease }];
    return;
  }
  const existing = clip[lane].find((k) => k.at_ms === at);
  if (existing) existing.value = v;
  else clip[lane] = tidy([...clip[lane], { at_ms: at, value: v, ease }], MAX_KEYFRAMES);
}

/** Add a key at clip time `t` holding the lane's current value (the "◆" button). */
export function addKey(p: Project, clipId: number, lane: Lane, t: number): void {
  const found = findClip(advancedOf(p), clipId);
  if (!found) return;
  const at = Math.min(Math.max(0, Math.round(t)), clipLength(found.clip));
  const keys = lane === 'zoom' || lane === 'x' || lane === 'y' ? found.clip.transform : found.clip[lane];
  // The first key of a lane: also pin the value at 0 so the start doesn't change.
  if (keys.length === 0 && at > 0) setLane(p, clipId, lane, 0, laneValue(found.clip, lane, 0));
  setLane(p, clipId, lane, at, laneValue(found.clip, lane, at));
}

/** Remove the key at clip time `t` (for transform lanes the whole zoom/x/y key). */
export function removeKey(p: Project, clipId: number, lane: Lane, t: number): void {
  const found = findClip(advancedOf(p), clipId);
  if (!found) return;
  const { clip } = found;
  if (lane === 'zoom' || lane === 'x' || lane === 'y') clip.transform = clip.transform.filter((k) => k.at_ms !== t);
  else clip[lane] = clip[lane].filter((k) => k.at_ms !== t);
}

/** Move a key from clip time `from` to `to` (not onto another key). */
export function moveKey(p: Project, clipId: number, lane: Lane, from: number, to: number): void {
  const found = findClip(advancedOf(p), clipId);
  if (!found) return;
  const { clip } = found;
  const at = Math.min(Math.max(0, Math.round(to)), clipLength(clip));
  const transform = lane === 'zoom' || lane === 'x' || lane === 'y';
  const keys: { at_ms: number }[] = transform ? clip.transform : clip[lane as 'opacity' | 'volume'];
  if (at !== from && keys.some((k) => k.at_ms === at)) return;
  const key = keys.find((k) => k.at_ms === from);
  if (!key) return;
  key.at_ms = at;
  if (transform) clip.transform = tidy(clip.transform, MAX_KEYFRAMES);
  else clip[lane as 'opacity' | 'volume'] = tidy(clip[lane as 'opacity' | 'volume'], MAX_KEYFRAMES);
}

/** Clear a lane: no keys, the neutral value. */
export function clearLane(p: Project, clipId: number, lane: Lane): void {
  const found = findClip(advancedOf(p), clipId);
  if (!found) return;
  if (lane === 'zoom' || lane === 'x' || lane === 'y') found.clip.transform = [];
  else found.clip[lane] = [];
}

/** The speed (percent) at clip time `t`. */
export function rateAt(c: PlacedClip, t: number): number {
  return [...c.speed].reverse().find((s) => s.at_ms <= t)?.rate_pct ?? 100;
}

/**
 * From clip time `t` on the clip plays at `ratePct` (until the next point). The clip
 * keeps its source range, so its length on the timeline changes.
 */
export function setSpeedPoint(p: Project, clipId: number, t: number, ratePct: number): void {
  const found = findClip(advancedOf(p), clipId);
  if (!found) return;
  const { clip } = found;
  if (clip.source.kind === 'image') throw new EditError('bad_range', 'A picture has no speed');
  const at = Math.min(Math.max(0, Math.round(t)), Math.max(0, clipLength(clip) - 1));
  const rate = Math.round(clamp(ratePct, RATE_MIN, RATE_MAX));
  let points = clip.speed.length === 0 && at > 0 ? [{ at_ms: 0, rate_pct: 100 }] : [...clip.speed];
  points = points.filter((s) => s.at_ms !== at);
  points.push({ at_ms: at, rate_pct: rate });
  points = tidy(points, MAX_SPEED_POINTS);
  clip.speed = points.every((s) => s.rate_pct === 100) ? [] : points;
}

export function removeSpeedPoint(p: Project, clipId: number, t: number): void {
  const found = findClip(advancedOf(p), clipId);
  if (!found) return;
  const { clip } = found;
  clip.speed = clip.speed.filter((s) => s.at_ms !== t);
  if (clip.speed.every((s) => s.rate_pct === 100)) clip.speed = [];
}

/** The clip after `clipId` on its track, if any (where a transition goes). */
export function nextOnTrack(a: Advanced, clipId: number): PlacedClip | null {
  const found = findClip(a, clipId);
  if (!found) return null;
  return found.track.clips[found.index + 1] ?? null;
}

export function transitionAfter(a: Advanced, trackId: number, clipId: number): Transition | null {
  return a.transitions.find((t) => t.track === trackId && t.after_clip === clipId) ?? null;
}

/** Set the transition between `clipId` and the next clip on its track ('cut' removes it). */
export function setTransition(p: Project, clipId: number, kind: TransitionKind, msLen: number): void {
  const a = advancedOf(p);
  const found = findClip(a, clipId);
  if (!found) return;
  const trackId = found.track.id;
  a.transitions = a.transitions.filter((t) => !(t.track === trackId && t.after_clip === clipId));
  if (kind === 'cut') return;
  if (!nextOnTrack(a, clipId)) throw new EditError('bad_range', 'A transition needs a clip after this one on the track');
  if (a.transitions.length >= MAX_TRANSITIONS) throw new EditError('too_many_pieces', 'Too many transitions');
  a.transitions.push({ track: trackId, after_clip: clipId, kind, ms: Math.round(clamp(msLen, 1, MAX_TRANSITION_MS)) });
}

export function addMarker(p: Project, atMs: number, title: string): void {
  const a = advancedOf(p);
  if (a.markers.length >= MAX_MARKERS) throw new EditError('too_many_pieces', 'Too many markers');
  a.markers.push({ at_ms: ms(atMs), title: cleanTitle(title) || 'Marker', kind: 'user' });
  a.markers.sort((x, y) => x.at_ms - y.at_ms);
}

/** Change the user marker at index `i` of the project's own list. */
export function updateMarker(p: Project, i: number, patch: Partial<Pick<Marker, 'at_ms' | 'title'>>): void {
  const a = advancedOf(p);
  const m = a.markers[i];
  if (!m) return;
  if (patch.at_ms !== undefined) m.at_ms = ms(patch.at_ms);
  if (patch.title !== undefined) m.title = cleanTitle(patch.title) || 'Marker';
  a.markers.sort((x, y) => x.at_ms - y.at_ms);
}

export function removeMarker(p: Project, i: number): void {
  const a = advancedOf(p);
  a.markers.splice(i, 1);
}

export function setColour(p: Project, patch: Partial<Colour>): void {
  const a = advancedOf(p);
  const c = { ...a.colour, ...patch };
  a.colour = { brightness: clamp(c.brightness, -1, 1), contrast: clamp(c.contrast, 0, 2), saturation: clamp(c.saturation, 0, 2) };
}

export function setDucking(p: Project, patch: Partial<Ducking>): void {
  const a = advancedOf(p);
  const d = { ...a.ducking, ...patch };
  a.ducking = {
    enabled: !!d.enabled,
    duck_db: clamp(d.duck_db, -40, 0),
    attack_ms: Math.round(clamp(d.attack_ms, 0, 5_000)),
    release_ms: Math.round(clamp(d.release_ms, 0, 10_000)),
  };
}

export function setTrack(p: Project, trackId: number, patch: Partial<Pick<Track, 'name' | 'muted'>>): void {
  const t = advancedOf(p).tracks.find((x) => x.id === trackId);
  if (!t) return;
  if (patch.name !== undefined) t.name = cleanTitle(patch.name);
  if (patch.muted !== undefined) t.muted = patch.muted;
}

/** Move a track up (towards the bottom of the picture: draw order follows the list). */
export function moveTrack(p: Project, trackId: number, by: -1 | 1): void {
  const a = advancedOf(p);
  const i = a.tracks.findIndex((t) => t.id === trackId);
  const j = i + by;
  if (i < 0 || j < 0 || j >= a.tracks.length) return;
  const [t] = a.tracks.splice(i, 1);
  a.tracks.splice(j, 0, t!);
}

export function removeTrack(p: Project, trackId: number): void {
  const a = advancedOf(p);
  a.tracks = a.tracks.filter((t) => t.id !== trackId);
  a.transitions = a.transitions.filter((t) => t.track !== trackId);
}

/** A source's label for the timeline: the clip's name, the file's name. */
export function sourceLabel(s: Source, mainName: string, names: Map<string, string>): string {
  if (s.kind === 'main') return mainName;
  if (s.kind === 'library') return names.get(s.key) ?? 'Missing clip';
  const parts = s.path.split(/[\\/]/);
  return parts[parts.length - 1] || s.path;
}
