/**
 * Pure helpers for the Editor workspace: what the panels show (presets, speeds,
 * formats), the crop window, filmstrip geometry and the templates. No state: see
 * `editor.svelte.ts`. Unit-tested in `editor.test.ts`.
 */

import type { FormatKind, Position, Project, Speed, TextLayer } from './timeline';
import { MAX_TEXT_CHARS, MAX_TEXT_LAYERS, outputDurationMs } from './timeline';

/** What `editor.project.get` says about the clip. */
export interface SourceInfo {
  width: number;
  height: number;
  duration_ms: number;
  fps: number | null;
  bookmarks_ms: number[];
}

export interface TextPreset {
  id: string;
  label: string;
  hint: string;
  position: Position;
}

/** The engine's `klip_edit::text::PRESETS`, in menu order. */
export const TEXT_PRESETS: TextPreset[] = [
  { id: 'bold', label: 'Bold', hint: 'White, black outline, shadow', position: 'middle' },
  { id: 'pill', label: 'Pill', hint: 'White on a dark pill', position: 'bottom' },
  { id: 'caption', label: 'Caption', hint: 'Yellow, uppercase, outline', position: 'bottom' },
  { id: 'title', label: 'Title', hint: 'Uppercase on a dark box', position: 'top' },
];

export const POSITIONS: { value: Position; label: string }[] = [
  { value: 'top', label: 'Top' },
  { value: 'middle', label: 'Middle' },
  { value: 'bottom', label: 'Bottom' },
];

export const FORMATS: { value: FormatKind; label: string }[] = [
  { value: 'wide', label: '16:9' },
  { value: 'tall', label: '9:16' },
];

export const SPEED_OPTIONS: { value: Speed; label: string }[] = [
  { value: 0.5, label: '0.5x' },
  { value: 1, label: '1x' },
  { value: 2, label: '2x' },
];

export type ExportPreset = 'original' | 'youtube' | 'tiktok' | 'discord';

export const EXPORT_PRESETS: { id: ExportPreset; label: string; detail: string }[] = [
  { id: 'original', label: 'Original', detail: 'The clip’s size, rate, codec and bitrate' },
  { id: 'youtube', label: 'YouTube', detail: 'Up to 1080p · 60 fps · 20 Mbit/s' },
  { id: 'tiktok', label: 'TikTok', detail: 'Fits 1080x1920 · 60 fps · 12 Mbit/s H.264' },
  { id: 'discord', label: 'Discord', detail: 'H.264 under the Discord size limit' },
];

/** How long a new text cue lasts. */
export const TEXT_CUE_MS = 3_000;

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

function even(v: number): number {
  return Math.max(0, Math.trunc(v)) & ~1;
}

/** `klip_edit::Format::crop_rect`: the kept part of a `w x h` source frame. */
export function cropRect(kind: FormatKind, cropX: number, w: number, h: number): Rect {
  const full = { x: 0, y: 0, width: w, height: h };
  if (kind === 'wide') return full;
  const target = even((h * 9) / 16);
  if (target >= w || target === 0) return full;
  const spare = w - target;
  const x = even(Math.round(spare * Math.min(1, Math.max(0, Number.isFinite(cropX) ? cropX : 0.5))));
  return { x, y: 0, width: target, height: h };
}

/** The output size for a preset: the crop, scaled down to fit the preset's box. */
export function outputSize(kind: FormatKind, cropX: number, w: number, h: number): { width: number; height: number } {
  const r = cropRect(kind, cropX, w, h);
  return { width: Math.max(2, even(r.width)), height: Math.max(2, even(r.height)) };
}

/** Clamp an output or source time to `0..=max` whole ms. */
export function clampMs(v: number, max: number): number {
  if (!Number.isFinite(v)) return 0;
  return Math.min(Math.max(0, Math.round(v)), Math.max(0, max));
}

/** Pointer x on a strip of `width` px → source ms. */
export function pxToMs(x: number, width: number, durationMs: number): number {
  if (width <= 0) return 0;
  return clampMs((x / width) * durationMs, durationMs);
}

/** Source ms → left edge in percent of the strip. */
export function msToPct(ms: number, durationMs: number): number {
  if (durationMs <= 0) return 0;
  return Math.min(100, Math.max(0, (ms / durationMs) * 100));
}

/** Which of `tiles` strip tiles shows source time `ms`. */
export function tileFor(ms: number, durationMs: number, tiles: number): number {
  if (durationMs <= 0 || tiles <= 0) return 0;
  return Math.min(tiles - 1, Math.max(0, Math.floor((ms / durationMs) * tiles)));
}

/** The source ranges the project throws away (shaded on the strip). */
export function gaps(p: Project): { start_ms: number; end_ms: number }[] {
  const kept = p.pieces.length ? p.pieces : p.source_ms > 0 ? [{ start_ms: 0, end_ms: p.source_ms }] : [];
  const out: { start_ms: number; end_ms: number }[] = [];
  let at = 0;
  for (const piece of kept) {
    if (piece.start_ms > at) out.push({ start_ms: at, end_ms: piece.start_ms });
    at = Math.max(at, piece.end_ms);
  }
  if (p.source_ms > at) out.push({ start_ms: at, end_ms: p.source_ms });
  return out;
}

export function nextTextId(p: Project): number {
  return p.text.reduce((m, t) => Math.max(m, t.id), 0) + 1;
}

/** A new text layer at output time `atMs` (clipped to the edit). */
export function newText(p: Project, presetId: string, atMs: number): TextLayer | null {
  if (p.text.length >= MAX_TEXT_LAYERS) return null;
  const preset = TEXT_PRESETS.find((t) => t.id === presetId) ?? TEXT_PRESETS[0]!;
  const duration = outputDurationMs(p);
  if (duration <= 0) return null;
  const start = clampMs(atMs, Math.max(0, duration - 1));
  return {
    id: nextTextId(p),
    preset: preset.id,
    text: 'Your text',
    position: preset.position,
    start_ms: start,
    end_ms: Math.min(duration, start + TEXT_CUE_MS),
  };
}

/** What `clean_text` keeps, enough to show the count. */
export function trimText(s: string): string {
  return Array.from(s.replace(/\r\n?/g, '\n'))
    .filter((c) => c === '\n' || !/\p{Cc}/u.test(c))
    .slice(0, MAX_TEXT_CHARS)
    .join('');
}

/** The "Shorts" template (`klip_edit::template`): tall, centred, the name as a caption. */
export function applyShorts(p: Project, clipName: string): Project {
  const next: Project = { ...p, format: { kind: 'tall', crop_x: 0.5 } };
  next.pieces = p.pieces.map((piece) => ({ ...piece, speed: 1 }));
  const duration = outputDurationMs(next);
  const caption: TextLayer = {
    id: nextTextId(p),
    preset: 'caption',
    text: trimText(clipName) || 'Clip',
    position: 'bottom',
    start_ms: 0,
    end_ms: Math.min(duration || TEXT_CUE_MS, TEXT_CUE_MS),
  };
  next.text = [...p.text.filter((t) => t.preset !== 'caption' || t.start_ms !== 0), caption].slice(0, MAX_TEXT_LAYERS);
  return next;
}

/** `(0.5x)` style badge for a piece, empty at 1x. */
export function speedBadge(speed: Speed): string {
  return speed === 1 ? '' : `${speed}x`;
}

/** Percent done of an export pass. */
export function progressPct(done: number, total: number): number {
  if (!(total > 0)) return 0;
  return Math.min(100, Math.max(0, Math.round((done / total) * 100)));
}
