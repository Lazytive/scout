/**
 * The editor's project model and timeline math, a port of `crates/klip-edit`
 * (`project.rs`, `timeline.rs`): output time ↔ source time over pieces at 0.5x / 1x / 2x,
 * and the Normal-mode edits (split, trim, cut, speed). Integer ms everywhere, the same
 * rounding as Rust, checked by `timeline.test.ts` against `crates/klip-edit/fixtures`:
 * the preview seeks where the export cuts.
 */

export type Speed = 0.5 | 1 | 2;
export type FormatKind = 'wide' | 'tall';
export type Position = 'top' | 'middle' | 'bottom';
export type Mode = 'normal' | 'advanced';

export interface Piece {
  start_ms: number;
  end_ms: number;
  speed: Speed;
}

export interface Format {
  kind: FormatKind;
  /** Where the 9:16 window sits: 0 = left edge, 0.5 = centre, 1 = right edge. */
  crop_x: number;
}

export interface TextLayer {
  id: number;
  preset: string;
  text: string;
  position: Position;
  /** Output time. */
  start_ms: number;
  end_ms: number;
}

export interface Volumes {
  game: number;
  mic: number;
}

/** Schema v1. Unknown keys (Advanced mode, newer versions) ride along untouched. */
export interface Project {
  version: number;
  mode: Mode;
  source_ms: number;
  pieces: Piece[];
  format: Format;
  text: TextLayer[];
  volumes: Volumes;
  [extra: string]: unknown;
}

export interface Segment {
  src_start_ms: number;
  src_end_ms: number;
  speed: Speed;
  out_start_ms: number;
  out_end_ms: number;
}

export const MAX_PIECES = 256;
export const MAX_TEXT_LAYERS = 64;
export const MAX_TEXT_CHARS = 200;
export const MAX_VOLUME = 2;
export const SPEEDS: Speed[] = [0.5, 1, 2];

export type EditErrorKind = 'unknown_duration' | 'bad_range' | 'nothing_left' | 'too_many_pieces';

export class EditError extends Error {
  constructor(public readonly kind: EditErrorKind) {
    super(
      {
        unknown_duration: "the clip's length isn't known yet",
        bad_range: 'the range is empty or outside the edit',
        nothing_left: 'that would remove the whole clip',
        too_many_pieces: `too many pieces (at most ${MAX_PIECES})`,
      }[kind],
    );
  }
}

export function newProject(sourceMs: number): Project {
  return {
    version: 1,
    mode: 'normal',
    source_ms: Math.max(0, Math.trunc(sourceMs)),
    pieces: [],
    format: { kind: 'wide', crop_x: 0.5 },
    text: [],
    volumes: { game: 1, mic: 1 },
  };
}

/** A deep copy (projects are plain JSON). */
export function cloneProject(p: Project): Project {
  return JSON.parse(JSON.stringify(p)) as Project;
}

/** Source ms per `den` output ms. */
function ratio(speed: Speed): [number, number] {
  if (speed === 0.5) return [1, 2];
  if (speed === 2) return [2, 1];
  return [1, 1];
}

function outLen(srcLen: number, speed: Speed): number {
  const [num, den] = ratio(speed);
  return Math.floor((srcLen * den + num - 1) / num);
}

function srcOffset(outOff: number, speed: Speed): number {
  const [num, den] = ratio(speed);
  return Math.floor((outOff * num) / den);
}

function outOffset(srcOff: number, speed: Speed): number {
  const [num, den] = ratio(speed);
  return Math.floor((srcOff * den) / num);
}

/** The pieces as played: the project's, or the whole clip when it has none. */
export function pieces(p: Project): Piece[] {
  if (p.pieces.length > 0) return p.pieces;
  if (p.source_ms === 0) return [];
  return [{ start_ms: 0, end_ms: p.source_ms, speed: 1 }];
}

/** The pieces placed back to back on the output timeline. */
export function segments(p: Project): Segment[] {
  const out: Segment[] = [];
  let at = 0;
  for (const piece of pieces(p)) {
    const len = outLen(piece.end_ms - piece.start_ms, piece.speed);
    out.push({
      src_start_ms: piece.start_ms,
      src_end_ms: piece.end_ms,
      speed: piece.speed,
      out_start_ms: at,
      out_end_ms: at + len,
    });
    at += len;
  }
  return out;
}

export function outputDurationMs(p: Project): number {
  const segs = segments(p);
  return segs.length ? segs[segs.length - 1]!.out_end_ms : 0;
}

/** The source time shown at output time `outMs`; `null` at or after the end. */
export function sourceTime(p: Project, outMs: number): number | null {
  const seg = segments(p).find((s) => outMs >= s.out_start_ms && outMs < s.out_end_ms);
  return seg ? seg.src_start_ms + srcOffset(outMs - seg.out_start_ms, seg.speed) : null;
}

/** When source time `srcMs` is shown; `null` if it is cut or past the end. */
export function outputTime(p: Project, srcMs: number): number | null {
  const seg = segments(p).find((s) => srcMs >= s.src_start_ms && srcMs < s.src_end_ms);
  return seg ? seg.out_start_ms + outOffset(srcMs - seg.src_start_ms, seg.speed) : null;
}

function materialize(p: Project): void {
  if (p.pieces.length === 0) {
    if (p.source_ms === 0) throw new EditError('unknown_duration');
    p.pieces = pieces(p);
  }
}

/**
 * Put a piece boundary at output time `outMs` (the picture doesn't change). Returns
 * whether a new boundary was made.
 */
export function split(p: Project, outMs: number): boolean {
  materialize(p);
  const duration = outputDurationMs(p);
  if (outMs > duration) throw new EditError('bad_range');
  if (outMs === duration) return false;
  const segs = segments(p);
  const i = segs.findIndex((s) => outMs >= s.out_start_ms && outMs < s.out_end_ms);
  const seg = segs[i];
  if (!seg) throw new EditError('bad_range');
  if (outMs === seg.out_start_ms) return false;
  const src = seg.src_start_ms + srcOffset(outMs - seg.out_start_ms, seg.speed);
  if (src <= seg.src_start_ms || src >= seg.src_end_ms) return false;
  if (p.pieces.length >= MAX_PIECES) throw new EditError('too_many_pieces');
  const piece = p.pieces[i];
  if (!piece) throw new EditError('bad_range');
  const first: Piece = { start_ms: piece.start_ms, end_ms: src, speed: piece.speed };
  const second: Piece = { start_ms: src, end_ms: piece.end_ms, speed: piece.speed };
  p.pieces.splice(i, 1, first, second);
  return true;
}

function boundary(p: Project, outMs: number): number {
  const segs = segments(p);
  const seg = segs.find((s) => outMs >= s.out_start_ms && outMs < s.out_end_ms);
  if (seg) return seg.out_start_ms;
  return segs.length ? segs[segs.length - 1]!.out_end_ms : 0;
}

/** Split at both ends and return the piece index range `[first, last)` in between. */
function isolate(p: Project, outStart: number, outEnd: number): [number, number] {
  if (outStart >= outEnd) throw new EditError('bad_range');
  materialize(p);
  if (outEnd > outputDurationMs(p)) throw new EditError('bad_range');
  split(p, outStart);
  split(p, outEnd);
  const a = boundary(p, outStart);
  const b = boundary(p, outEnd);
  const segs = segments(p);
  const first = segs.findIndex((s) => s.out_start_ms >= a);
  if (first < 0) throw new EditError('bad_range');
  let last = first;
  for (let i = segs.length - 1; i >= 0; i--) {
    if (segs[i]!.out_end_ms <= b) {
      last = Math.max(first, i + 1);
      break;
    }
  }
  return [first, last];
}

/** Keep only output `outStart..outEnd`. */
export function trim(p: Project, outStart: number, outEnd: number): void {
  const before = p.pieces.map((x) => ({ ...x }));
  let range: [number, number];
  try {
    range = isolate(p, outStart, outEnd);
  } catch (e) {
    p.pieces = before;
    throw e;
  }
  const [first, last] = range;
  if (first >= last) {
    p.pieces = before;
    throw new EditError('nothing_left');
  }
  p.pieces = p.pieces.filter((_, i) => i >= first && i < last);
}

/** Remove output `outStart..outEnd`. */
export function cut(p: Project, outStart: number, outEnd: number): void {
  const before = p.pieces.map((x) => ({ ...x }));
  let range: [number, number];
  try {
    range = isolate(p, outStart, outEnd);
  } catch (e) {
    p.pieces = before;
    throw e;
  }
  const [first, last] = range;
  const kept = p.pieces.filter((_, i) => i < first || i >= last);
  if (kept.length === 0) {
    p.pieces = before;
    throw new EditError('nothing_left');
  }
  p.pieces = kept;
}

/** Play output `outStart..outEnd` at `speed`. */
export function setSpeed(p: Project, outStart: number, outEnd: number, speed: Speed): void {
  const before = p.pieces.map((x) => ({ ...x }));
  let range: [number, number];
  try {
    range = isolate(p, outStart, outEnd);
  } catch (e) {
    p.pieces = before;
    throw e;
  }
  const [first, last] = range;
  p.pieces = p.pieces.map((piece, i) => (i >= first && i < last ? { ...piece, speed } : piece));
}

/** The project as the engine returns it, with the fields this build knows filled in. */
export function asProject(v: unknown): Project {
  const o = (v && typeof v === 'object' ? v : {}) as Record<string, unknown>;
  const base = newProject(typeof o.source_ms === 'number' ? o.source_ms : 0);
  const p: Project = { ...o, ...base };
  for (const k of ['version', 'mode', 'source_ms', 'pieces', 'format', 'text', 'volumes'] as const) {
    if (o[k] !== undefined) (p as Record<string, unknown>)[k] = o[k];
  }
  if (!Array.isArray(p.pieces)) p.pieces = [];
  if (!Array.isArray(p.text)) p.text = [];
  return p;
}
