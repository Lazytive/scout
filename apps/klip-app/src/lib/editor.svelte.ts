/**
 * Editor state (ADR 0017, M8.5; Advanced mode M9.5): the clip being edited, its project
 * (saved to the engine after every edit, debounced), the preview's media (the clip, its
 * sound tracks, and for Advanced mode every placed clip and file), the playhead and
 * selection on the output timeline, labelled undo, and the export in flight. Pure math
 * lives in `timeline.ts`, `editor.ts`, `advanced.ts` and `compose.ts`; the panels are
 * `src/editor/*.svelte` and `src/editor/advanced/*`.
 */

import type { AudioTrack } from './api';
import { CommandError, call, editorAudio, editorFiles, editorMedia, fileUrl } from './api';
import type { ExportPreset, SourceInfo } from './editor';
import { applyShorts, clampMs, newText } from './editor';
import type { LibClip } from './library';
import { lib, refreshLibrary, refreshSoon } from './library.svelte';
import { app } from './store.svelte';
import type { Cue } from '../editor/advanced/player';
import { compose } from './compose';
import { moveClip, readAdvanced } from './advanced';
import type { Project, Speed, TextLayer } from './timeline';
import { EditError, asProject, cloneProject, cut, outputDurationMs, outputTime, setSpeed, split, trim } from './timeline';
import type { KlipEvent } from './types';

export type MediaStatus = 'none' | 'loading' | 'original' | 'ready' | 'making' | 'error';

export interface ExportRun {
  id: number;
  preset: string;
  out: string;
  pass: number;
  frames_done: number;
  frames_total: number;
}

/** A library clip the project places, as the engine lists it. */
export interface PlacedInfo {
  key: string;
  name: string;
  status: 'ok' | 'missing' | 'unreadable';
  source: SourceInfo | null;
  path: string | null;
}

/** What the preview has for one source (`player.ts` `sourceKey`). */
export interface SourceEntry {
  url: string | null;
  tracks: { name: string; url: string }[];
  image: string | null;
  status: 'ready' | 'making' | 'none' | 'error';
}

/** A Custom export's numbers (`export.start` `custom`). */
export interface CustomExport {
  width: number;
  height: number;
  fps: number;
  bitrate_kbps: number;
  codec: 'h264' | 'hevc' | 'av1';
}

export interface ExportResult {
  id: number;
  preset: string;
  ok: boolean;
  path: string | null;
  error: string | null;
  summary: unknown;
}

/** Time the project is saved after the last edit, ms. */
const SAVE_AFTER = 400;
const MAX_UNDO = 50;

export const ed = $state({
  clip: null as LibClip | null,
  tiles: 16,
  project: null as Project | null,
  source: null as SourceInfo | null,
  status: 'new' as string,
  issues: [] as string[],
  loading: false,
  saving: false,
  dirty: false,
  error: null as string | null,
  media: { status: 'none' as MediaStatus, url: null as string | null, path: null as string | null },
  /** Output time, ms. */
  playhead: 0,
  playing: false,
  /** A selected output range, ms. */
  selection: null as { start: number; end: number } | null,
  /** The text layer being edited. */
  textId: null as number | null,
  exportOpen: false,
  exporting: null as ExportRun | null,
  exportResult: null as ExportResult | null,
  exportError: null as string | null,
  undo: [] as Project[],
  redo: [] as Project[],
  /** What each undo step did, parallel to `undo`. */
  undoLabels: [] as string[],
  redoLabels: [] as string[],
  /** Other library clips the project places (a montage). */
  clips: [] as PlacedInfo[],
  /** The main clip's sound tracks (empty: the video's own sound). */
  tracks: [] as AudioTrack[],
  /** Advanced mode's media by source key. */
  sources: {} as Record<string, SourceEntry>,
  /** Bumped when media became playable. */
  mediaVersion: 0,
  /** The selected clip of an Advanced track (the one the panels show). */
  clipId: null as number | null,
  /** Every selected clip (a multi-selection); only counts while it holds `clipId`. */
  selected: [] as number[],
  /** The timeline's tool: Selection (V) or Razor (C). */
  tool: 'select' as 'select' | 'razor',
  /** Main (V1/A1) can't be edited on the Advanced timeline until unlocked; locked on every open. */
  mainLocked: true,
  /** The selected piece of Main on the timeline (an index into its segments), once unlocked. */
  mainSel: null as number | null,
  /** The selected transition of an Advanced track (the clip it follows). */
  transitionSel: null as { track: number; after: number } | null,
  /** Effect Controls groups whose stopwatch is on this session (`<clip id>:<group>`). */
  animating: {} as Record<string, boolean>,
  /** The text cues as the engine draws them (both modes' previews). */
  cues: [] as Cue[],
});

let saveTimer: ReturnType<typeof setTimeout> | null = null;

function describe(e: unknown): string {
  if (e instanceof CommandError) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

interface ProjectReply {
  key: string;
  path: string;
  project: unknown;
  issues: { path: string; message: string }[];
  status: string;
  source: SourceInfo;
  clips?: PlacedInfo[];
}

function take(r: ProjectReply): void {
  ed.project = asProject(r.project);
  ed.source = r.source;
  ed.status = r.status;
  ed.issues = r.issues.map((i) => i.message);
  const before = JSON.stringify(ed.clips.map((c) => [c.key, c.status]));
  ed.clips = Array.isArray(r.clips) ? r.clips : [];
  const duration = editDuration();
  ed.playhead = clampMs(ed.playhead, Math.max(0, duration - 1));
  if (JSON.stringify(ed.clips.map((c) => [c.key, c.status])) !== before) void loadSources();
}

/** The edit's length: Normal mode's cut, or the whole composition in Advanced mode. */
export function editDuration(): number {
  const p = ed.project;
  const s = ed.source;
  if (!p) return 0;
  if (!s) return outputDurationMs(p);
  return compose(p, s, placedInfos()).duration_ms;
}

/** The placed library clips as `compose` wants them. */
export function placedInfos(): [string, SourceInfo][] {
  return ed.clips.flatMap((c) => (c.source ? [[c.key, c.source] as [string, SourceInfo]] : []));
}

/** Open the editor on a clip. */
export function openEditor(clip: LibClip): void {
  if (ed.clip?.key !== clip.key) {
    ed.project = null;
    ed.source = null;
    ed.playhead = 0;
    ed.selection = null;
    ed.textId = null;
    ed.clipId = null;
    ed.selected = [];
    ed.tool = 'select';
    ed.transitionSel = null;
    ed.animating = {};
    ed.undo = [];
    ed.redo = [];
    ed.undoLabels = [];
    ed.redoLabels = [];
    ed.clips = [];
    ed.tracks = [];
    ed.sources = {};
    ed.media = { status: 'none', url: null, path: null };
    ed.exportResult = null;
  }
  ed.clip = clip;
  // Main is locked again on every open (the user's choice), even the same clip.
  ed.mainLocked = true;
  ed.mainSel = null;
  ed.tiles = lib.listing?.strip.tiles ?? 16;
  ed.error = null;
  ed.playing = false;
  app.page = 'editor';
  void loadProject();
  void requestMedia();
}

/** Open the editor on a clip path (test override `KLIP_START_CLIP`). */
export async function openEditorByPath(path: string): Promise<void> {
  await refreshLibrary();
  // The library's spelling may differ in slashes and case (Windows).
  const norm = (p: string) => p.replaceAll('\\', '/').toLowerCase();
  const clip = lib.listing?.clips.find((c) => norm(c.path) === norm(path));
  if (clip) openEditor(clip);
  else {
    app.page = 'library';
    app.error = `No clip at ${path}`;
  }
}

export function closeEditor(): void {
  void flushSave();
  ed.playing = false;
  app.page = 'library';
}

export async function loadProject(): Promise<void> {
  const clip = ed.clip;
  if (!clip) return;
  ed.loading = true;
  try {
    const r = await call<ProjectReply>('editor.project.get', { path: clip.path });
    if (ed.clip?.key === clip.key) {
      take(r);
      // The project's files and placed clips, for Advanced mode's preview.
      void loadSources();
    }
  } catch (e) {
    ed.error = describe(e);
  } finally {
    ed.loading = false;
  }
}

async function saveNow(): Promise<void> {
  const clip = ed.clip;
  const project = ed.project;
  if (!clip || !project) return;
  ed.saving = true;
  try {
    const r = await call<ProjectReply>('editor.project.set', { path: clip.path, project });
    if (ed.clip?.key === clip.key && !ed.dirty) take(r);
    ed.error = null;
  } catch (e) {
    ed.error = describe(e);
  } finally {
    ed.saving = false;
  }
}

function scheduleSave(): void {
  ed.dirty = true;
  if (saveTimer) clearTimeout(saveTimer);
  saveTimer = setTimeout(() => {
    saveTimer = null;
    ed.dirty = false;
    void saveNow();
  }, SAVE_AFTER);
}

/** Save what is pending right now. */
export async function flushSave(): Promise<void> {
  if (!saveTimer) return;
  clearTimeout(saveTimer);
  saveTimer = null;
  ed.dirty = false;
  await saveNow();
}

function pushUndo(current: Project, label: string): void {
  ed.undo = [...ed.undo.slice(-(MAX_UNDO - 1)), current];
  ed.undoLabels = [...ed.undoLabels.slice(-(MAX_UNDO - 1)), label];
  ed.redo = [];
  ed.redoLabels = [];
}

function settle(next: Project): void {
  ed.project = next;
  ed.error = null;
  const duration = editDuration();
  ed.playhead = clampMs(ed.playhead, Math.max(0, duration - 1));
  if (ed.selection && ed.selection.end > outputDurationMs(next)) ed.selection = null;
}

/** Apply `change` to a copy of the project; a thrown `EditError` leaves it as it was. */
export function edit(change: (p: Project) => void, label = 'Edit'): boolean {
  flushLive();
  const current = ed.project;
  if (!current) return false;
  const next = cloneProject(current);
  try {
    change(next);
  } catch (e) {
    ed.error = e instanceof EditError ? e.message : describe(e);
    return false;
  }
  pushUndo(current, label);
  settle(next);
  scheduleSave();
  return true;
}

/** A drag in progress: changes apply to the project as it was when it started. */
let gestureBase: Project | null = null;

/** Start a drag (one undo step for the whole gesture). */
export function beginGesture(): void {
  gestureBase = ed.project ? cloneProject(ed.project) : null;
}

/** Show the drag's change so far; a refused change keeps the last good one. */
export function gesture(change: (p: Project) => void): boolean {
  if (!gestureBase) beginGesture();
  if (!gestureBase) return false;
  const next = cloneProject(gestureBase);
  try {
    change(next);
  } catch {
    return false;
  }
  settle(next);
  return true;
}

/** End the drag: one undo step when it changed anything. */
export function endGesture(label: string): void {
  const base = gestureBase;
  gestureBase = null;
  if (!base || !ed.project) return;
  if (JSON.stringify(base) === JSON.stringify(ed.project)) return;
  pushUndo(base, label);
  scheduleSave();
}

let liveTimer: ReturnType<typeof setTimeout> | null = null;
let liveLabel = '';

/** End a resting slider drag now (before another edit or an undo). */
function flushLive(): void {
  if (!liveTimer) return;
  clearTimeout(liveTimer);
  liveTimer = null;
  endGesture(liveLabel);
}

/**
 * A change made while a control is being dragged (a slider): shown at once, one undo
 * step once it rests. `change` must set absolute values.
 */
export function liveEdit(change: (p: Project) => void, label: string): void {
  if (liveTimer) clearTimeout(liveTimer);
  else beginGesture();
  liveLabel = label;
  gesture(change);
  liveTimer = setTimeout(() => {
    liveTimer = null;
    endGesture(liveLabel);
  }, 500);
}

/** Undo `steps` times (the history panel jumps several at once). */
export function undo(steps = 1): void {
  flushLive();
  for (let i = 0; i < steps; i++) {
    const prev = ed.undo.at(-1);
    if (!prev || !ed.project) break;
    ed.redo = [...ed.redo, ed.project];
    ed.redoLabels = [...ed.redoLabels, ed.undoLabels.at(-1) ?? 'Edit'];
    ed.undo = ed.undo.slice(0, -1);
    ed.undoLabels = ed.undoLabels.slice(0, -1);
    ed.project = prev;
  }
  scheduleSave();
}

export function redo(steps = 1): void {
  flushLive();
  for (let i = 0; i < steps; i++) {
    const next = ed.redo.at(-1);
    if (!next || !ed.project) break;
    ed.undo = [...ed.undo, ed.project];
    ed.undoLabels = [...ed.undoLabels, ed.redoLabels.at(-1) ?? 'Edit'];
    ed.redo = ed.redo.slice(0, -1);
    ed.redoLabels = ed.redoLabels.slice(0, -1);
    ed.project = next;
  }
  scheduleSave();
}

/** What a timeline edit of locked Main says. */
export const MAIN_LOCKED = 'Main is locked: click the lock on V1 to edit it here';

/**
 * The selected Advanced clips: `ed.selected` while it holds the primary `ed.clipId`, else
 * just that one (code that sets `ed.clipId` alone makes a single selection).
 */
export function selectedClips(): number[] {
  if (ed.clipId === null) return [];
  return ed.selected.includes(ed.clipId) ? ed.selected : [ed.clipId];
}

/** Select `ids`; `primary` (default: the last) is the one the panels show. */
export function selectClips(ids: number[], primary: number | null = ids.at(-1) ?? null): void {
  const list = ids.filter((id, i) => ids.indexOf(id) === i);
  ed.clipId = primary !== null && list.includes(primary) ? primary : (list.at(-1) ?? null);
  ed.selected = list;
  ed.mainSel = null;
  ed.textId = null;
  ed.transitionSel = null;
}

export function seek(outMs: number): void {
  ed.playhead = clampMs(outMs, Math.max(0, editDuration() - 1));
}

export function select(start: number, end: number): void {
  const duration = ed.project ? outputDurationMs(ed.project) : 0;
  const a = clampMs(Math.min(start, end), duration);
  const b = clampMs(Math.max(start, end), duration);
  ed.selection = b > a ? { start: a, end: b } : null;
}

export function splitAtPlayhead(): void {
  edit((p) => {
    split(p, ed.playhead);
  }, 'Split');
}

export function cutSelection(): void {
  const s = ed.selection;
  if (!s) return;
  if (edit((p) => cut(p, s.start, s.end), 'Cut')) {
    ed.selection = null;
    seek(s.start);
  }
}

export function trimToSelection(): void {
  const s = ed.selection;
  if (!s) return;
  if (edit((p) => trim(p, s.start, s.end), 'Keep selection')) {
    ed.selection = null;
    seek(0);
  }
}

export function speedSelection(speed: Speed): void {
  const s = ed.selection;
  if (!s) return;
  edit((p) => setSpeed(p, s.start, s.end, speed), `Speed ${speed}x`);
}

/** Move the start of the kept range (source ms). */
export function trimStart(srcMs: number): void {
  edit((p) => {
    if (p.pieces.length === 0) p.pieces = [{ start_ms: 0, end_ms: p.source_ms, speed: 1 }];
    const first = p.pieces[0];
    if (!first) return;
    first.start_ms = clampMs(srcMs, first.end_ms - 1);
  }, 'Trim start');
}

/** Move the end of the kept range (source ms). */
export function trimEnd(srcMs: number): void {
  edit((p) => {
    if (p.pieces.length === 0) p.pieces = [{ start_ms: 0, end_ms: p.source_ms, speed: 1 }];
    const last = p.pieces[p.pieces.length - 1];
    if (!last) return;
    last.end_ms = Math.max(last.start_ms + 1, clampMs(srcMs, p.source_ms));
  }, 'Trim end');
}

export function setFormat(kind: 'wide' | 'tall'): void {
  edit((p) => {
    p.format.kind = kind;
  }, kind === 'tall' ? 'Format 9:16' : 'Format 16:9');
}

export function setCropX(x: number): void {
  edit((p) => {
    p.format.crop_x = Math.min(1, Math.max(0, x));
  }, 'Crop position');
}

export function setVolumes(game: number, mic: number): void {
  edit((p) => {
    p.volumes = { game: Math.min(2, Math.max(0, game)), mic: Math.min(2, Math.max(0, mic)) };
  }, 'Sound');
}

export function addText(presetId: string): void {
  let id: number | null = null;
  edit((p) => {
    const t = newText(p, presetId, ed.playhead);
    if (!t) throw new EditError('too_many_pieces');
    p.text.push(t);
    id = t.id;
  }, 'Add text');
  if (id !== null) ed.textId = id;
}

export function updateText(id: number, patch: Partial<TextLayer>): void {
  edit((p) => {
    const t = p.text.find((x) => x.id === id);
    if (!t) return;
    Object.assign(t, patch);
    const duration = outputDurationMs(p);
    t.start_ms = clampMs(t.start_ms, Math.max(0, duration - 1));
    t.end_ms = clampMs(t.end_ms, duration);
    if (t.end_ms <= t.start_ms) t.end_ms = Math.min(duration, t.start_ms + 500);
  }, 'Edit text');
}

export function removeText(id: number): void {
  edit((p) => {
    p.text = p.text.filter((t) => t.id !== id);
  }, 'Remove text');
  if (ed.textId === id) ed.textId = null;
}

export function applyTemplate(name: 'shorts'): void {
  const clip = ed.clip;
  if (!clip || name !== 'shorts') return;
  edit((p) => {
    Object.assign(p, applyShorts(p, clip.name));
  }, 'Shorts template');
}

/** Output time of a source time, or the nearest kept one after it. */
export function outputOf(srcMs: number): number | null {
  const p = ed.project;
  if (!p) return null;
  const direct = outputTime(p, srcMs);
  if (direct !== null) return direct;
  for (const piece of p.pieces) {
    if (piece.start_ms >= srcMs) return outputTime(p, piece.start_ms);
  }
  return null;
}

interface MediaReply {
  status: 'original' | 'ready' | 'making';
  path?: string;
  id?: number;
  key: string;
}

/** Ask what the preview should play and make it reachable. */
export async function requestMedia(): Promise<void> {
  const clip = ed.clip;
  if (!clip) return;
  ed.media = { status: 'loading', url: null, path: null };
  void loadTracks(clip.path, clip.key);
  try {
    const r = (await editorMedia(clip.path)) as MediaReply & { url?: string };
    if (ed.clip?.key !== clip.key) return;
    if (r.status === 'making') {
      ed.media = { status: 'making', url: null, path: null };
      return;
    }
    ed.media = { status: r.status, url: r.url ?? null, path: r.path ?? null };
    ed.mediaVersion += 1;
  } catch (e) {
    ed.media = { status: 'error', url: null, path: null };
    ed.error = describe(e);
  }
}

/** The main clip's sound tracks for the preview's mixer (none: the video's own sound). */
async function loadTracks(path: string, key: string): Promise<void> {
  try {
    const tracks = await editorAudio(path);
    if (ed.clip?.key !== key) return;
    ed.tracks = tracks.filter((t) => !!t.url);
    // Advanced mode's player made its elements without them: make them again.
    ed.mediaVersion += 1;
  } catch {
    if (ed.clip?.key === key) ed.tracks = [];
  }
}

/** Sources being looked up now (not state: nothing shows it). */
const loading: Record<string, boolean> = {};

/** One placed library clip's media: what plays (the clip or its proxy) and its sound tracks. */
async function loadClipSource(c: PlacedInfo, force = false): Promise<void> {
  const id = `lib:${c.key}`;
  if (!c.path || c.status !== 'ok' || loading[id] || (!force && ed.sources[id]?.status === 'ready')) return;
  loading[id] = true;
  const key = ed.clip?.key;
  try {
    const m = await editorMedia(c.path);
    const tracks = (await editorAudio(c.path).catch(() => [] as AudioTrack[])).filter((t) => !!t.url) as { name: string; url: string }[];
    if (ed.clip?.key !== key) return;
    ed.sources[id] = { url: m.url ?? null, tracks, image: null, status: m.status === 'making' ? 'making' : 'ready' };
  } catch {
    if (ed.clip?.key === key) ed.sources[id] = { url: null, tracks: [], image: null, status: 'error' };
  } finally {
    delete loading[id];
  }
}

/** Advanced mode's media: every placed library clip and the project's files. */
export async function loadSources(): Promise<void> {
  const clip = ed.clip;
  if (!clip) return;
  for (const c of ed.clips) await loadClipSource(c);
  try {
    const files = await editorFiles(clip.path);
    if (ed.clip?.key !== clip.key) return;
    for (const f of files) {
      const id = `${f.kind === 'image' ? 'image' : 'file'}:${f.path}`;
      const url = f.url ?? null;
      ed.sources[id] = {
        url: f.kind === 'audio' ? url : null,
        tracks: [],
        image: f.kind === 'image' ? url : null,
        status: f.ok ? 'ready' : 'error',
      };
    }
  } catch {
    // The files show as missing.
  }
  ed.mediaVersion += 1;
}

/** A file the user just picked (the dialog lets the page read it) as a source. */
export async function adoptFile(kind: 'image' | 'audio', path: string): Promise<string | null> {
  const url = await fileUrl(path);
  const id = `${kind === 'image' ? 'image' : 'file'}:${path}`;
  ed.sources[id] = { url: kind === 'audio' ? url : null, tracks: [], image: kind === 'image' ? url : null, status: url ? 'ready' : 'none' };
  ed.mediaVersion += 1;
  return url;
}

/** Append library clips to the montage (`editor.montage`). */
export async function addToMontage(paths: string[]): Promise<void> {
  const clip = ed.clip;
  if (!clip || paths.length === 0) return;
  await flushSave();
  const before = ed.project ? cloneProject(ed.project) : null;
  try {
    const r = await call<ProjectReply>('editor.montage', { path: clip.path, add: paths });
    if (ed.clip?.key !== clip.key) return;
    if (before) pushUndo(before, paths.length === 1 ? 'Add a clip' : `Add ${paths.length} clips`);
    take(r);
    await loadSources();
  } catch (e) {
    ed.error = describe(e);
  }
}

/**
 * Add one library clip where it was dropped on the timeline: the engine appends it (and
 * learns its source), then it moves to `atMs` on `track` (an existing video track, or
 * 'new'), all one undo step. A refused move leaves it at the end of the edit.
 */
export async function addToMontageAt(path: string, atMs: number, track: number | 'new'): Promise<void> {
  const clip = ed.clip;
  if (!clip) return;
  await flushSave();
  const before = ed.project ? cloneProject(ed.project) : null;
  const known = before ? readAdvanced(before).tracks.flatMap((t) => t.clips.map((c) => c.id)) : [];
  try {
    const r = await call<ProjectReply>('editor.montage', { path: clip.path, add: [path] });
    if (ed.clip?.key !== clip.key) return;
    if (before) pushUndo(before, 'Add a clip');
    take(r);
    const added = ed.project ? readAdvanced(ed.project).tracks.flatMap((t) => t.clips).find((c) => !known.includes(c.id)) : undefined;
    if (added && ed.project) {
      const next = cloneProject(ed.project);
      try {
        moveClip(next, added.id, Math.max(0, Math.round(atMs)), track);
        settle(next);
        ed.clipId = added.id;
        scheduleSave();
      } catch {
        // Doesn't fit there: it stays where the engine put it.
      }
    }
    await loadSources();
  } catch (e) {
    ed.error = describe(e);
  }
}

export interface Snapshot {
  id: number;
  saved_unix_ms: number;
  bytes: number;
}

export async function listHistory(): Promise<Snapshot[]> {
  const clip = ed.clip;
  if (!clip) return [];
  await flushSave();
  try {
    const r = await call<{ snapshots: Snapshot[] }>('editor.history.list', { path: clip.path });
    return Array.isArray(r.snapshots) ? r.snapshots : [];
  } catch (e) {
    ed.error = describe(e);
    return [];
  }
}

export async function restoreHistory(id: number): Promise<void> {
  const clip = ed.clip;
  if (!clip) return;
  await flushSave();
  const before = ed.project ? cloneProject(ed.project) : null;
  try {
    const r = await call<ProjectReply>('editor.history.restore', { path: clip.path, id });
    if (ed.clip?.key !== clip.key) return;
    if (before) pushUndo(before, 'Restore a saved version');
    take(r);
  } catch (e) {
    ed.error = describe(e);
  }
}

export async function startExport(preset: ExportPreset | 'custom', custom?: CustomExport): Promise<void> {
  const clip = ed.clip;
  if (!clip) return;
  await flushSave();
  ed.exportError = null;
  ed.exportResult = null;
  try {
    const args: Record<string, unknown> = { path: clip.path, preset };
    if (preset === 'custom') args.custom = custom;
    const r = await call<{ id: number; out: string; preset: string }>('export.start', args);
    ed.exporting = { id: r.id, preset: r.preset, out: r.out, pass: 1, frames_done: 0, frames_total: 0 };
  } catch (e) {
    ed.exportError = describe(e);
  }
}

export async function cancelExport(): Promise<void> {
  const run = ed.exporting;
  if (!run) return;
  try {
    await call('export.cancel', { id: run.id });
  } catch (e) {
    ed.exportError = describe(e);
  }
}

export async function revealPath(path: string): Promise<void> {
  try {
    await call('clip.reveal', { path });
  } catch (e) {
    ed.exportError = describe(e);
  }
}

/** Export and proxy events (called from the store's dispatcher). */
export function editorEvent(e: KlipEvent): void {
  if (e.event === 'export_progress') {
    if (e.preset === 'proxy') return;
    if (ed.exporting?.id === e.id || !ed.exporting) {
      ed.exporting = {
        id: e.id,
        preset: e.preset,
        out: ed.exporting?.out ?? '',
        pass: e.pass,
        frames_done: e.frames_done,
        frames_total: e.frames_total,
      };
    }
    return;
  }
  if (e.event === 'export_done') {
    if (e.preset === 'proxy') {
      if (ed.clip?.key === e.key) void requestMedia();
      const placed = ed.clips.find((c) => c.key === e.key);
      if (placed) void loadClipSource(placed, true).then(() => (ed.mediaVersion += 1));
      return;
    }
    ed.exporting = null;
    // A refusal from while it ran ("an export is already running") is stale now.
    ed.exportError = null;
    ed.exportResult = { id: e.id, preset: e.preset, ok: e.ok, path: e.path, error: e.error, summary: e.summary };
    if (e.ok) refreshSoon(300);
  }
}
