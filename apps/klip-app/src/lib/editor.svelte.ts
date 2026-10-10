/**
 * Editor state (ADR 0017, M8.5): the clip being edited, its project (saved to the
 * engine after every edit, debounced), the preview's media, the playhead and selection
 * on the output timeline, and the export in flight. Pure math lives in `timeline.ts`
 * and `editor.ts`; the panels are `src/editor/*.svelte`.
 */

import { CommandError, call, editorMedia } from './api';
import type { ExportPreset, SourceInfo } from './editor';
import { applyShorts, clampMs, newText } from './editor';
import type { LibClip } from './library';
import { lib, refreshLibrary, refreshSoon } from './library.svelte';
import { app } from './store.svelte';
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
}

function take(r: ProjectReply): void {
  ed.project = asProject(r.project);
  ed.source = r.source;
  ed.status = r.status;
  ed.issues = r.issues.map((i) => i.message);
  const duration = outputDurationMs(ed.project);
  ed.playhead = clampMs(ed.playhead, Math.max(0, duration - 1));
}

/** Open the editor on a clip. */
export function openEditor(clip: LibClip): void {
  if (ed.clip?.key !== clip.key) {
    ed.project = null;
    ed.source = null;
    ed.playhead = 0;
    ed.selection = null;
    ed.textId = null;
    ed.undo = [];
    ed.redo = [];
    ed.media = { status: 'none', url: null, path: null };
    ed.exportResult = null;
  }
  ed.clip = clip;
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
  const clip = lib.listing?.clips.find((c) => c.path === path);
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
    if (ed.clip?.key === clip.key) take(r);
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

/** Apply `change` to a copy of the project; a thrown `EditError` leaves it as it was. */
export function edit(change: (p: Project) => void): boolean {
  const current = ed.project;
  if (!current) return false;
  const next = cloneProject(current);
  try {
    change(next);
  } catch (e) {
    ed.error = e instanceof EditError ? e.message : describe(e);
    return false;
  }
  ed.undo = [...ed.undo.slice(-(MAX_UNDO - 1)), current];
  ed.redo = [];
  ed.project = next;
  ed.error = null;
  const duration = outputDurationMs(next);
  ed.playhead = clampMs(ed.playhead, Math.max(0, duration - 1));
  if (ed.selection && ed.selection.end > duration) ed.selection = null;
  scheduleSave();
  return true;
}

export function undo(): void {
  const prev = ed.undo.at(-1);
  if (!prev || !ed.project) return;
  ed.redo = [...ed.redo, ed.project];
  ed.undo = ed.undo.slice(0, -1);
  ed.project = prev;
  scheduleSave();
}

export function redo(): void {
  const next = ed.redo.at(-1);
  if (!next || !ed.project) return;
  ed.undo = [...ed.undo, ed.project];
  ed.redo = ed.redo.slice(0, -1);
  ed.project = next;
  scheduleSave();
}

export function seek(outMs: number): void {
  const duration = ed.project ? outputDurationMs(ed.project) : 0;
  ed.playhead = clampMs(outMs, Math.max(0, duration - 1));
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
  });
}

export function cutSelection(): void {
  const s = ed.selection;
  if (!s) return;
  if (edit((p) => cut(p, s.start, s.end))) {
    ed.selection = null;
    seek(s.start);
  }
}

export function trimToSelection(): void {
  const s = ed.selection;
  if (!s) return;
  if (edit((p) => trim(p, s.start, s.end))) {
    ed.selection = null;
    seek(0);
  }
}

export function speedSelection(speed: Speed): void {
  const s = ed.selection;
  if (!s) return;
  edit((p) => setSpeed(p, s.start, s.end, speed));
}

/** Move the start of the kept range (source ms). */
export function trimStart(srcMs: number): void {
  edit((p) => {
    if (p.pieces.length === 0) p.pieces = [{ start_ms: 0, end_ms: p.source_ms, speed: 1 }];
    const first = p.pieces[0];
    if (!first) return;
    first.start_ms = clampMs(srcMs, first.end_ms - 1);
  });
}

/** Move the end of the kept range (source ms). */
export function trimEnd(srcMs: number): void {
  edit((p) => {
    if (p.pieces.length === 0) p.pieces = [{ start_ms: 0, end_ms: p.source_ms, speed: 1 }];
    const last = p.pieces[p.pieces.length - 1];
    if (!last) return;
    last.end_ms = Math.max(last.start_ms + 1, clampMs(srcMs, p.source_ms));
  });
}

export function setFormat(kind: 'wide' | 'tall'): void {
  edit((p) => {
    p.format.kind = kind;
  });
}

export function setCropX(x: number): void {
  edit((p) => {
    p.format.crop_x = Math.min(1, Math.max(0, x));
  });
}

export function setVolumes(game: number, mic: number): void {
  edit((p) => {
    p.volumes = { game: Math.min(2, Math.max(0, game)), mic: Math.min(2, Math.max(0, mic)) };
  });
}

export function addText(presetId: string): void {
  let id: number | null = null;
  edit((p) => {
    const t = newText(p, presetId, ed.playhead);
    if (!t) throw new EditError('too_many_pieces');
    p.text.push(t);
    id = t.id;
  });
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
  });
}

export function removeText(id: number): void {
  edit((p) => {
    p.text = p.text.filter((t) => t.id !== id);
  });
  if (ed.textId === id) ed.textId = null;
}

export function applyTemplate(name: 'shorts'): void {
  const clip = ed.clip;
  if (!clip || name !== 'shorts') return;
  edit((p) => {
    Object.assign(p, applyShorts(p, clip.name));
  });
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
  try {
    const r = (await editorMedia(clip.path)) as MediaReply & { url?: string };
    if (ed.clip?.key !== clip.key) return;
    if (r.status === 'making') {
      ed.media = { status: 'making', url: null, path: null };
      return;
    }
    ed.media = { status: r.status, url: r.url ?? null, path: r.path ?? null };
  } catch (e) {
    ed.media = { status: 'error', url: null, path: null };
    ed.error = describe(e);
  }
}

export async function startExport(preset: ExportPreset): Promise<void> {
  const clip = ed.clip;
  if (!clip) return;
  await flushSave();
  ed.exportError = null;
  ed.exportResult = null;
  try {
    const r = await call<{ id: number; out: string; preset: string }>('export.start', { path: clip.path, preset });
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
      return;
    }
    ed.exporting = null;
    // A refusal from while it ran ("an export is already running") is stale now.
    ed.exportError = null;
    ed.exportResult = { id: e.id, preset: e.preset, ok: e.ok, path: e.path, error: e.error, summary: e.summary };
    if (e.ok) refreshSoon(300);
  }
}
