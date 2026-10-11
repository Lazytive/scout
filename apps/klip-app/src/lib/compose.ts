/**
 * From a project to what is drawn and heard, a port of `klip_edit::compose` (ADR 0018):
 * the main clip as Normal mode cut it is track 0, Advanced tracks lie above it in
 * order, transitions become overlaps and opacity ramps, chapters of every placed clip
 * become markers. Checked against `crates/klip-edit/fixtures/advanced/compose.json` by
 * `compose.test.ts`, so the preview's canvas draws the layers the export draws.
 */

import type { Ducking, Marker, MarkerKind, Source, TrackKind } from './advanced';
import { FROM_BELOW, MAX_MARKERS, MAX_SOURCE_MS, readAdvanced } from './advanced';
import type { Rect, SourceInfo } from './editor';
import { cropRect } from './editor';
import type { Colour, Keyframe, PictureKind, SpeedPoint, Transform, TransformKey } from './keyframes';
import { outputLength, sourceOffset, transformAt, valueAt } from './keyframes';
import type { Project, Segment, TextLayer, Volumes } from './timeline';
import { MAX_VOLUME, segments } from './timeline';

export const MAX_LAYERS = 4096;

export type LayerKind = PictureKind;

export interface Layer {
  id: number;
  /** 0 = the main clip as Normal mode cut it; else the Advanced track's id. */
  track: number;
  kind: LayerKind;
  source: Source;
  src_in_ms: number;
  src_out_ms: number;
  at_ms: number;
  end_ms: number;
  z: number;
  /** No sound (a muted track). */
  muted: boolean;
  /** No picture (a hidden track); its sound still plays unless muted. */
  hidden: boolean;
  transform: TransformKey[];
  opacity: Keyframe[];
  volume: Keyframe[];
  speed: SpeedPoint[];
}

export interface Composition {
  duration_ms: number;
  layers: Layer[];
  texts: TextLayer[];
  main_volumes: Volumes;
  colour: Colour;
  source: SourceInfo;
  crop: Rect;
  markers: Marker[];
  ducking: Ducking;
}

export function layerActive(l: Layer, outMs: number): boolean {
  return outMs >= l.at_ms && outMs < l.end_ms;
}

/** The source time shown at output time `outMs`, or `null` outside the layer. */
export function layerSourceMs(l: Layer, outMs: number): number | null {
  if (!layerActive(l, outMs)) return null;
  const src = l.src_in_ms + sourceOffset(l.speed, outMs - l.at_ms);
  return Math.min(src, Math.max(0, l.src_out_ms - 1));
}

/** The first output time that shows source time `srcMs`, or `null`. */
export function layerOutputMs(l: Layer, srcMs: number): number | null {
  if (srcMs < l.src_in_ms || srcMs >= l.src_out_ms || l.end_ms <= l.at_ms) return null;
  const want = srcMs - l.src_in_ms;
  let lo = 0;
  let hi = l.end_ms - l.at_ms;
  while (lo < hi) {
    const mid = lo + Math.floor((hi - lo) / 2);
    if (sourceOffset(l.speed, mid) >= want) hi = mid;
    else lo = mid + 1;
  }
  const out = l.at_ms + lo;
  return layerActive(l, out) ? out : null;
}

export function layerTransformAt(l: Layer, outMs: number): Transform {
  return transformAt(l.transform, Math.max(0, outMs - l.at_ms)) ?? { zoom: 1, x: 0, y: 0 };
}

export function layerOpacityAt(l: Layer, outMs: number): number {
  const v = valueAt(l.opacity, Math.max(0, outMs - l.at_ms)) ?? 1;
  return Math.min(1, Math.max(0, v));
}

export function layerVolumeAt(l: Layer, outMs: number): number {
  if (l.muted) return 0;
  const v = valueAt(l.volume, Math.max(0, outMs - l.at_ms)) ?? 1;
  return Math.min(MAX_VOLUME, Math.max(0, v));
}

/** The playback rate (1 = realtime) of a layer at output time `outMs`. */
export function layerRateAt(l: Layer, outMs: number): number {
  const local = outMs - l.at_ms;
  let rate = 100;
  for (const p of l.speed) if (p.at_ms <= local) rate = p.rate_pct;
  return Math.max(1, rate) / 100;
}

/** Picture layers at `outMs`, bottom first. */
export function visibleAt(c: Composition, outMs: number): Layer[] {
  return c.layers
    .filter((l) => (l.kind === 'video' || l.kind === 'image' || l.kind === 'black') && !l.hidden && layerActive(l, outMs))
    .sort((a, b) => a.z - b.z || a.id - b.id);
}

/** Layers with sound at `outMs` (the main clip's own Game/Mic is `main_volumes`). */
export function audibleAt(c: Composition, outMs: number): Layer[] {
  return c.layers.filter((l) => (l.kind === 'audio' || l.kind === 'music' || l.kind === 'video') && layerActive(l, outMs) && !l.muted);
}

export function textsAt(c: Composition, outMs: number): TextLayer[] {
  return c.texts.filter((t) => outMs >= t.start_ms && outMs < t.end_ms);
}

function rateOf(speed: number): number {
  return speed === 0.5 ? 50 : speed === 2 ? 200 : 100;
}

function kindOf(track: TrackKind, source: Source): LayerKind {
  if (source.kind === 'image') return 'image';
  if (track === 'music') return 'music';
  if (track === 'audio') return 'audio';
  return 'video';
}

function fade(fromMs: number, toMs: number, from: number, to: number): Keyframe[] {
  return [
    { at_ms: fromMs, value: from, ease: 'linear' },
    { at_ms: toMs, value: to, ease: 'linear' },
  ];
}

/**
 * A dip's black over `from..until`: it fades in up to `blackAt` (where the clip before
 * ends), stays black to `backAt` (where the next one starts) and fades out by `until`.
 */
function blackLayer(id: number, track: number, z: number, [muted, hidden]: [boolean, boolean], from: number, blackAt: number, backAt: number, until: number): Layer {
  const key = (t: number, value: number): Keyframe => ({ at_ms: Math.max(0, t - from), value, ease: 'linear' });
  const opacity = [key(from, 0), key(blackAt, 1)];
  if (backAt > blackAt) opacity.push(key(backAt, 1));
  opacity.push(key(until, 0));
  return {
    id,
    track,
    kind: 'black',
    source: { kind: 'main' },
    src_in_ms: 0,
    src_out_ms: 0,
    at_ms: from,
    end_ms: until,
    z,
    muted,
    hidden,
    transform: [],
    opacity,
    volume: [],
    speed: [],
  };
}

function chapters(info: SourceInfo): [number, string | null][] {
  const events = info.events ?? [];
  const titleAt = (ms: number) => events.find((e) => e.at_ms === ms && e.title !== '')?.title ?? null;
  const out: [number, string | null][] = (info.bookmarks_ms ?? []).slice(0, MAX_MARKERS).map((ms) => [ms, titleAt(ms)]);
  for (const e of events.slice(0, MAX_MARKERS)) {
    if (!(info.bookmarks_ms ?? []).includes(e.at_ms) && e.title !== '') out.push([e.at_ms, e.title]);
  }
  return out;
}

function chapterMarker(at: number, title: string | null): Marker {
  return title !== null ? { at_ms: at, title, kind: 'event' } : { at_ms: at, title: '', kind: 'bookmark' };
}

function outputTimeIn(segs: Segment[], srcMs: number): number | null {
  const s = segs.find((x) => srcMs >= x.src_start_ms && srcMs < x.src_end_ms);
  if (!s) return null;
  const off = srcMs - s.src_start_ms;
  const den = s.speed === 0.5 ? 2 : 1;
  const num = s.speed === 2 ? 2 : 1;
  return s.out_start_ms + Math.floor((off * den) / num);
}

const RANK: Record<MarkerKind, number> = { bookmark: 0, event: 1, user: 2 };

function byteCompare(a: string, b: string): number {
  // Rust compares strings by UTF-8 bytes; code point order is the same.
  const x = Array.from(a);
  const y = Array.from(b);
  for (let i = 0; i < Math.min(x.length, y.length); i++) {
    const d = x[i]!.codePointAt(0)! - y[i]!.codePointAt(0)!;
    if (d !== 0) return d;
  }
  return x.length - y.length;
}

/**
 * `compose_with`: the composition of `project` over `source` (the main clip as probed);
 * `clips` are the other library clips it places, by key.
 */
export function compose(project: Project, source: SourceInfo, clips: [string, SourceInfo][] = []): Composition {
  const p: Project = { ...project };
  if (source.duration_ms > 0 && source.duration_ms <= MAX_SOURCE_MS) p.source_ms = source.duration_ms;
  const adv = readAdvanced(p);
  const segs = segments(p);
  const layers: Layer[] = [];
  let nextId = 1;
  // V1 arranged on the Advanced timeline replaces Normal mode's cut (M9.8.9).
  const arranged = adv.tracks.some((t) => t.main);
  for (const seg of arranged ? [] : segs) {
    layers.push({
      id: nextId++,
      track: 0,
      kind: 'video',
      source: { kind: 'main' },
      src_in_ms: seg.src_start_ms,
      src_out_ms: seg.src_end_ms,
      at_ms: seg.out_start_ms,
      end_ms: seg.out_end_ms,
      z: 0,
      muted: false,
      hidden: false,
      transform: [],
      opacity: [],
      volume: [],
      speed: [{ at_ms: 0, rate_pct: rateOf(seg.speed) }],
    });
  }
  let duration = !arranged && segs.length ? segs[segs.length - 1]!.out_end_ms : 0;
  adv.tracks.forEach((track, ti) => {
    const z = track.main ? 0 : ti + 1;
    let previous: [number, number] | null = null;
    for (const clip of track.clips) {
      if (layers.length >= MAX_LAYERS) break;
      const srcLen = Math.max(0, clip.out_ms - clip.in_ms);
      let at = Math.min(clip.at_ms, MAX_SOURCE_MS);
      let opacity = clip.opacity.map((k) => ({ ...k }));
      // A dip on a picture track: where the black starts, where the clip before ends, its length.
      let dip: [number, number, number] | null = null;
      // The first clip may crossfade in from whatever is below it (`FROM_BELOW`).
      const after: number = previous ? previous[1] : FROM_BELOW;
      const transition = adv.transitions.find((t) => t.track === track.id && t.after_clip === after);
      if (transition) {
        const len = Math.min(transition.ms, srcLen);
        if (transition.kind === 'crossfade') {
          at = Math.max(0, at - len);
          if (opacity.length === 0) opacity = fade(0, len, 0, 1);
        } else if (transition.kind === 'dip' && previous) {
          const prev = layers[previous[0]];
          if (track.kind === 'video' || track.kind === 'text') {
            // Through black whatever is below: a black layer over the join, the clips as they are.
            if (prev && len > 0) dip = [prev.end_ms - Math.min(len, prev.end_ms - prev.at_ms), prev.end_ms, len];
          } else {
            if (prev && prev.opacity.length === 0) {
              const plen = prev.end_ms - prev.at_ms;
              prev.opacity = fade(Math.max(0, plen - len), plen, 1, 0);
            }
            if (opacity.length === 0) opacity = fade(0, len, 0, 1);
          }
        }
      }
      const outLen = outputLength(clip.speed, srcLen);
      const end = Math.min(at + outLen, MAX_SOURCE_MS * 2);
      if (end <= at) continue;
      layers.push({
        id: nextId,
        track: Math.max(1, track.id),
        kind: kindOf(track.kind, clip.source),
        source: clip.source,
        src_in_ms: clip.in_ms,
        src_out_ms: clip.out_ms,
        at_ms: at,
        end_ms: end,
        z,
        muted: track.muted,
        hidden: track.hidden,
        transform: clip.transform,
        opacity,
        volume: clip.volume,
        speed: clip.speed,
      });
      previous = [layers.length - 1, clip.id];
      nextId++;
      duration = Math.max(duration, end);
      if (dip && layers.length < MAX_LAYERS) {
        // Same z, a later id: over both clips, under the tracks above.
        layers.push(blackLayer(nextId, Math.max(1, track.id), z, [track.muted, track.hidden], dip[0], dip[1], at, at + Math.min(dip[2], outLen)));
        nextId++;
      }
    }
  });
  const mainChapters = chapters(source);
  const markers: Marker[] = [];
  // (Arranged, the main clip's chapters come with its layers below.)
  for (const [src, title] of arranged ? [] : mainChapters) {
    const at = outputTimeIn(segs, src);
    if (at !== null) markers.push(chapterMarker(at, title));
  }
  for (const l of layers) {
    if (l.track === 0 || l.kind === 'black') continue;
    let list: [number, string | null][];
    if (l.source.kind === 'main') list = mainChapters;
    else if (l.source.kind === 'library') {
      const key = l.source.key;
      const info = clips.find(([k]) => k === key)?.[1];
      if (!info) continue;
      list = chapters(info);
    } else continue;
    for (const [src, title] of list) {
      const at = layerOutputMs(l, src);
      if (at !== null) markers.push(chapterMarker(at, title));
    }
  }
  for (const m of adv.markers) if (m.at_ms < duration) markers.push({ ...m });
  markers.sort((a, b) => a.at_ms - b.at_ms || RANK[a.kind] - RANK[b.kind] || byteCompare(a.title, b.title));
  const deduped: Marker[] = [];
  for (const m of markers) {
    const last = deduped[deduped.length - 1];
    if (last && last.at_ms === m.at_ms && last.kind === m.kind && last.title === m.title) continue;
    deduped.push(m);
  }
  const kept = deduped.slice(0, MAX_MARKERS);
  let n = 0;
  for (const m of kept) {
    if (m.kind === 'bookmark') {
      n++;
      if (m.title === '') m.title = `Bookmark ${n}`;
    } else if (m.title === '') m.title = 'Marker';
  }
  return {
    duration_ms: duration,
    layers,
    texts: p.text.filter((t) => t.start_ms < t.end_ms && t.start_ms < duration),
    main_volumes: p.volumes,
    colour: adv.colour,
    source,
    crop: cropRect(p.format.kind, p.format.crop_x, source.width, source.height),
    markers: kept,
    ducking: adv.ducking,
  };
}

/** Whether any layer comes from beyond the main clip's own segments. */
export function hasAdvancedLayers(c: Composition): boolean {
  return c.layers.some((l) => l.track !== 0);
}
