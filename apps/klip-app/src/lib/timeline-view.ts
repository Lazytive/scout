// The Advanced timeline's geometry and gesture rules (M9.7.1), kept out of the component
// so they can be tested: row order and labels, the drag threshold, clip hit zones, the
// zoom bar, and where transitions sit (the same placement `compose` uses).
import type { Advanced, Track, TransitionKind } from './advanced';
import { FROM_BELOW, MAX_TRANSITION_MS, clipLength, overlapOf } from './advanced';

/** How far (px) the pointer must travel before a press on a clip moves or trims it. */
export const DRAG_PX = 4;
/** The widest trim zone at each end of a clip, px. */
export const EDGE_PX = 6;
/** The shortest transition a drag makes, ms. */
export const MIN_TRANSITION_MS = 100;
/** A new transition's length, ms. */
export const DEFAULT_TRANSITION_MS = 500;

export function pastThreshold(dx: number, dy: number, px = DRAG_PX): boolean {
  return Math.hypot(dx, dy) >= px;
}

/** Where a press at `x` px into a clip `width` px wide grabs it. */
export function clipZone(x: number, width: number): 'start' | 'move' | 'end' {
  const e = Math.min(EDGE_PX, width / 4);
  if (x < e) return 'start';
  if (x > width - e) return 'end';
  return 'move';
}

export type TimelineRow =
  | { kind: 'track'; track: Track; label: string; group: 'video' | 'sound' }
  | { kind: 'main'; label: string; group: 'video' }
  | { kind: 'main-sound'; label: string; group: 'sound' };

/**
 * Rows top to bottom, Premiere-style: the video tracks from the top of the picture down
 * (V2… in draw order), Main as V1, Main's sound as A1, then the sound tracks (A2…).
 */
export function rowsOf(a: Advanced): TimelineRow[] {
  const video = a.tracks.filter((t) => t.kind === 'video' || t.kind === 'text');
  const sound = a.tracks.filter((t) => t.kind === 'audio' || t.kind === 'music');
  const out: TimelineRow[] = [];
  for (let i = video.length - 1; i >= 0; i--) {
    const track = video[i];
    if (track) out.push({ kind: 'track', track, label: `V${i + 2}`, group: 'video' });
  }
  out.push({ kind: 'main', label: 'V1', group: 'video' });
  out.push({ kind: 'main-sound', label: 'A1', group: 'sound' });
  sound.forEach((track, i) => out.push({ kind: 'track', track, label: `A${i + 2}`, group: 'sound' }));
  return out;
}

// ---- zoom bar ----

/** The time the timeline spans: the edit plus a tail to drop clips after it, ms. */
export function spanMs(durationMs: number): number {
  return Math.max(0, durationMs) + Math.max(2000, Math.round(durationMs * 0.05));
}

/** Pixels per second that show the whole span. */
export function fitPps(span: number, viewW: number): number {
  return Math.max(0.5, viewW) / Math.max(0.001, span / 1000);
}

/** The visible part of the content as fractions (0…1) for the zoom bar's thumb. */
export function thumb(scrollLeft: number, viewW: number, contentW: number): { start: number; end: number } {
  if (contentW <= viewW || contentW <= 0) return { start: 0, end: 1 };
  const start = Math.min(1, Math.max(0, scrollLeft / contentW));
  return { start, end: Math.min(1, (scrollLeft + viewW) / contentW) };
}

/** The scroll position that keeps the time under `anchorX` (px into the view) in place. */
export function zoomAt(pps: number, newPps: number, anchorX: number, scrollLeft: number): number {
  const t = (scrollLeft + anchorX) / Math.max(0.001, pps);
  return Math.max(0, t * newPps - anchorX);
}

/** The zoom and scroll that show the span between the bar's handles at `f0`…`f1`. */
export function fromHandles(f0: number, f1: number, span: number, viewW: number, maxPps: number): { pps: number; scrollLeft: number } {
  const a = Math.min(1, Math.max(0, Math.min(f0, f1)));
  const b = Math.min(1, Math.max(0, Math.max(f0, f1)));
  const fit = fitPps(span, viewW);
  const visible = ((b - a) * span) / 1000;
  const pps = visible > 0 ? Math.min(maxPps, Math.max(fit, viewW / visible)) : maxPps;
  return { pps, scrollLeft: ((a * span) / 1000) * pps };
}

// ---- thumbnails and waveforms ----

/** The part of a clip (px from its left edge, `left` px into the lane, `w` wide) in view, with a margin. */
export function visibleSpan(left: number, w: number, scrollX: number, viewW: number, margin = 200): { a: number; b: number } {
  const a = Math.max(0, Math.floor(scrollX - left - margin));
  const b = Math.min(w, Math.ceil(scrollX + viewW - left + margin));
  return { a: Math.min(a, b), b };
}

/**
 * Back-to-back thumbnails over the visible part `a`…`b` of a clip `w` × `h` px showing
 * source `from`…`to` (ms): each shows the strip tile of the source time at its middle.
 */
export function thumbSlots(
  w: number,
  h: number,
  from: number,
  to: number,
  tiles: number,
  durationMs: number,
  a: number,
  b: number,
): { x: number; w: number; tile: number }[] {
  const th = h - 18;
  if (th < 14 || tiles < 1 || !(durationMs > 0) || !(w > 0)) return [];
  const tw = Math.max(16, Math.round((th * 16) / 9));
  const out: { x: number; w: number; tile: number }[] = [];
  for (let x = Math.floor(a / tw) * tw; x < Math.min(b, w - 2) && out.length < 200; x += tw) {
    const t = from + ((x + tw / 2) / w) * (to - from);
    const tile = Math.min(tiles - 1, Math.max(0, Math.floor((t / durationMs) * tiles)));
    out.push({ x, w: Math.min(tw, w - x), tile });
  }
  return out;
}

// ---- transitions ----

export interface TransitionBox {
  track: number;
  /** The clip the transition follows (its `after_clip`). */
  after: number;
  /** The clip it leads into. */
  next: number;
  kind: TransitionKind;
  ms: number;
  /** Where it is drawn, output ms. */
  start: number;
  end: number;
  /** The longest it may get (the next clip's source length), ms. */
  max: number;
}

/**
 * Every transition where `compose` puts it: a crossfade over the stretch where the next
 * clip is pulled under the one before, a dip centred on the cut.
 */
export function transitionBoxes(a: Advanced): TransitionBox[] {
  const out: TransitionBox[] = [];
  for (const tr of a.transitions) {
    if (tr.kind === 'cut') continue;
    const track = a.tracks.find((t) => t.id === tr.track);
    if (!track) continue;
    if (tr.after_clip === FROM_BELOW) {
      // Into the first clip, over whatever is below it.
      const first = track.clips[0];
      if (!first || tr.kind !== 'crossfade') continue;
      const srcLen = Math.max(0, first.out_ms - first.in_ms);
      const start = first.at_ms - overlapOf(a, track, first);
      out.push({ track: track.id, after: FROM_BELOW, next: first.id, kind: tr.kind, ms: tr.ms, start, end: start + Math.min(tr.ms, srcLen), max: Math.min(MAX_TRANSITION_MS, srcLen) });
      continue;
    }
    const i = track.clips.findIndex((c) => c.id === tr.after_clip);
    const prev = track.clips[i];
    const next = track.clips[i + 1];
    if (i < 0 || !prev || !next) continue;
    const srcLen = Math.max(0, next.out_ms - next.in_ms);
    const ms = Math.min(tr.ms, srcLen);
    let start: number;
    let end: number;
    if (tr.kind === 'crossfade') {
      start = next.at_ms - overlapOf(a, track, next);
      end = start + ms;
    } else {
      const prevEnd = prev.at_ms - overlapOf(a, track, prev) + clipLength(prev);
      start = prevEnd - ms;
      end = next.at_ms - overlapOf(a, track, next) + ms;
    }
    out.push({ track: track.id, after: prev.id, next: next.id, kind: tr.kind, ms: tr.ms, start, end, max: Math.min(MAX_TRANSITION_MS, srcLen) });
  }
  return out;
}

/** A transition's length after dragging one end by `dms` (start: left = longer). */
export function transitionLength(ms: number, edge: 'start' | 'end', dms: number, maxMs: number): number {
  const raw = edge === 'start' ? ms - dms : ms + dms;
  return Math.round(Math.min(Math.max(MIN_TRANSITION_MS, maxMs), Math.max(MIN_TRANSITION_MS, raw)));
}

/** Places where two clips of a track touch (within `tolMs`) without a transition. */
export function joins(a: Advanced, tolMs: number): { track: number; after: number; next: number; at: number }[] {
  const out: { track: number; after: number; next: number; at: number }[] = [];
  for (const t of a.tracks) {
    for (let i = 0; i + 1 < t.clips.length; i++) {
      const prev = t.clips[i];
      const next = t.clips[i + 1];
      if (!prev || !next) continue;
      if (a.transitions.some((x) => x.track === t.id && x.after_clip === prev.id && x.kind !== 'cut')) continue;
      const prevEnd = prev.at_ms - overlapOf(a, t, prev) + clipLength(prev);
      if (Math.abs(next.at_ms - prevEnd) <= tolMs) out.push({ track: t.id, after: prev.id, next: next.id, at: next.at_ms });
    }
  }
  return out;
}
