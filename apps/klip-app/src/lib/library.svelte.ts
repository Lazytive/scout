// Library state: the query, what `library.list` returned, the selection, and the actions
// on clips. Every action is an engine command (`library.*`, `clip.*`).

import { call, CommandError, dragFilesOut } from './api';
import { progressPct } from './editor';
import {
  copiedText,
  defaultQuery,
  discordReadyText,
  dragGroup,
  type DiscordResult,
  listArgs,
  pruneSelection,
  type LibClip,
  type Listing,
  type Query,
} from './library';
import { toast } from './toasts.svelte';
import type { KlipEvent, ThumbsReadyEvent } from './types';

export type CardSize = 's' | 'm' | 'l';

const SIZE_KEY = 'klip.library.size';

function loadSize(): CardSize {
  try {
    const v = localStorage.getItem(SIZE_KEY);
    if (v === 's' || v === 'm' || v === 'l') return v;
  } catch {
    // No storage (private mode): the default size.
  }
  return 'm';
}

export const lib = $state({
  query: { ...defaultQuery } as Query,
  listing: null as Listing | null,
  loading: false,
  /** The last failed action, shown in the toolbar until the next one. */
  error: null as string | null,
  selected: [] as string[],
  /** Where a Shift range starts. */
  anchor: null as string | null,
  /** The clip the arrow keys move from. */
  cursor: null as string | null,
  size: loadSize(),
  /** Rejected rename / tags, shown under the field. */
  renameError: null as string | null,
  tagError: null as string | null,
  /** A delete waiting for the user's answer. */
  confirm: null as null | { keys: string[]; permanent: boolean },
  /** The Library page was opened (only then do events refresh it). */
  active: false,
});

let seq = 0;

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export async function refreshLibrary(): Promise<void> {
  seq += 1;
  const mine = seq;
  lib.loading = true;
  try {
    const l = await call<Listing>('library.list', listArgs(lib.query, Date.now()));
    if (mine !== seq) return;
    lib.listing = l;
    lib.selected = pruneSelection(lib.selected, l.clips);
    if (lib.anchor && !lib.selected.includes(lib.anchor)) lib.anchor = lib.selected[0] ?? null;
  } catch (e) {
    if (mine === seq) lib.error = message(e);
  } finally {
    if (mine === seq) lib.loading = false;
  }
}

let refreshTimer: ReturnType<typeof setTimeout> | null = null;

/** Refresh soon (several events in a row make one listing). */
export function refreshSoon(ms = 150): void {
  if (refreshTimer) clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    void refreshLibrary();
  }, ms);
}

export function setQuery(patch: Partial<Query>): void {
  lib.query = { ...lib.query, ...patch };
  if ('search' in patch && Object.keys(patch).length === 1) refreshSoon();
  else void refreshLibrary();
}

export function setSize(size: CardSize): void {
  lib.size = size;
  try {
    localStorage.setItem(SIZE_KEY, size);
  } catch {
    // Not remembered; nothing else depends on it.
  }
}

export function clipByKey(key: string | null): LibClip | null {
  if (key === null) return null;
  return lib.listing?.clips.find((c) => c.key === key) ?? null;
}

export function selectedClips(): LibClip[] {
  const keys: Record<string, true> = Object.fromEntries(lib.selected.map((k) => [k, true]));
  return lib.listing?.clips.filter((c) => keys[c.key]) ?? [];
}

function thumbsReady(e: ThumbsReadyEvent): void {
  const l = lib.listing;
  const c = l?.clips.find((x) => x.key === e.key);
  if (!l || !c) return;
  const before = !!c.thumbs.poster && !!c.thumbs.strip;
  c.thumbs = { poster: e.poster, strip: e.strip };
  if (e.failed) c.error = c.error ?? "Couldn't make a thumbnail";
  if (!before && ((e.poster && e.strip) || e.failed)) l.thumbs_pending = Math.max(0, l.thumbs_pending - 1);
}

/** Engine events that change the library. */
export function libraryEvent(e: KlipEvent): void {
  if (e.event === 'export_progress' || e.event === 'export_done') discordEvent(e);
  if (!lib.active) return;
  switch (e.event) {
    case 'thumbs_ready':
      thumbsReady(e);
      break;
    case 'clip_saved':
    case 'storage_cleaned':
    case 'library_changed':
    case 'recording_stopped':
    case 'settings_changed':
      refreshSoon(300);
      break;
    default:
      break;
  }
}

async function act(fn: () => Promise<unknown>): Promise<boolean> {
  lib.error = null;
  try {
    await fn();
    return true;
  } catch (e) {
    lib.error = message(e);
    return false;
  } finally {
    await refreshLibrary();
  }
}

/** Star or unstar every clip; one that fails doesn't stop the rest (a toast says which). */
export async function setStar(clips: LibClip[], starred: boolean): Promise<void> {
  lib.error = null;
  const failed: { clip: LibClip; error: unknown }[] = [];
  for (const c of clips) {
    try {
      await call('clip.star', { path: c.path, starred });
    } catch (error) {
      failed.push({ clip: c, error });
    }
  }
  const first = failed[0];
  if (first) {
    const verb = starred ? 'star' : 'unstar';
    const what = clips.length === 1 ? 'the clip' : `${failed.length} of ${clips.length} clips`;
    toast(`Couldn't ${verb} ${what}: ${message(first.error)}`, 'error');
  }
  await refreshLibrary();
}

/** Rename a clip; on success it stays selected under its new key. */
export async function renameClip(c: LibClip, name: string): Promise<boolean> {
  lib.renameError = null;
  if (name.trim() === c.name) return true;
  try {
    const r = await call<{ key: string }>('clip.rename', {
      path: c.path,
      name,
    });
    lib.selected = lib.selected.map((k) => (k === c.key ? r.key : k));
    if (lib.anchor === c.key) lib.anchor = r.key;
    if (lib.cursor === c.key) lib.cursor = r.key;
    await refreshLibrary();
    return true;
  } catch (e) {
    lib.renameError = e instanceof CommandError ? e.message : message(e);
    return false;
  }
}

export async function setTags(c: LibClip, tags: string[]): Promise<void> {
  lib.tagError = null;
  try {
    const r = await call<{ tags: string[] }>('clip.tags', {
      path: c.path,
      tags,
    });
    c.tags = r.tags;
    void refreshLibrary();
  } catch (e) {
    lib.tagError = message(e);
  }
}

export function askDelete(keys: string[], permanent: boolean): void {
  if (keys.length > 0) lib.confirm = { keys, permanent };
}

export async function confirmDelete(): Promise<void> {
  const c = lib.confirm;
  lib.confirm = null;
  if (!c) return;
  const paths = c.keys.map((k) => clipByKey(k)?.path).filter((p): p is string => !!p);
  await act(async () => {
    const r = await call<{ failed: { path: string; error: string }[] }>('clip.delete', {
      paths,
      permanent: c.permanent,
    });
    if (r.failed.length > 0) {
      throw new Error(`${r.failed.length} clip(s) couldn't be deleted: ${r.failed[0]?.error ?? ''}`);
    }
  });
}

export async function openClip(c: LibClip, reveal = false): Promise<void> {
  lib.error = null;
  try {
    await call(reveal ? 'clip.reveal' : 'clip.open', { path: c.path });
  } catch (e) {
    lib.error = message(e);
  }
}

/** Put clips on the clipboard as copied files (M11.4); says so in a toast. */
export async function copyClips(clips: LibClip[]): Promise<boolean> {
  if (clips.length === 0) return false;
  try {
    await call('clip.copy', { paths: clips.map((c) => c.path) });
    toast(copiedText(clips.length));
    return true;
  } catch (e) {
    toast(`Couldn't copy: ${message(e)}`, 'error');
    return false;
  }
}

/** Copy a file Scout made (an export) by its path. */
export async function copyPath(path: string): Promise<boolean> {
  try {
    await call('clip.copy', { paths: [path] });
    toast(copiedText(1));
    return true;
  } catch (e) {
    toast(`Couldn't copy: ${message(e)}`, 'error');
    return false;
  }
}

/** Drag clips out of the window as files (M11.4): the selection when `c` is in it, else
 *  `c` alone (which becomes the selection), its poster under the pointer. */
export async function dragClipsOut(c: LibClip): Promise<void> {
  const keys = dragGroup(c.key, lib.selected);
  if (!lib.selected.includes(c.key)) {
    lib.selected = [c.key];
    lib.anchor = c.key;
    lib.cursor = c.key;
  }
  const paths = keys.map((k) => clipByKey(k)?.path).filter((p): p is string => !!p);
  if (paths.length === 0) return;
  let icon: string | null = null;
  if (c.thumbs.poster) {
    try {
      icon = (await call<{ path: string }>('library.thumb', { key: c.key, kind: 'poster' })).path;
    } catch {
      // No poster file yet: the plain drag picture.
    }
  }
  try {
    await dragFilesOut(paths, icon);
  } catch (e) {
    toast(`Couldn't drag: ${message(e)}`, 'error');
  }
}

/** The Library's "Discord" job (M11.5): the export making a copy of `key` that fits. */
export const discord = $state<{
  job: { key: string; id: number; pass: number; pct: number; reason: string } | null;
}>({ job: null });

/** Put a clip on the clipboard for Discord: as it is when it fits, else after an export
 *  with the Discord preset (the engine copies the result when it's done). */
export async function discordClip(c: LibClip): Promise<void> {
  if (discord.job) {
    toast('A Discord copy is already being made', 'error');
    return;
  }
  try {
    const r = await call<DiscordResult>('share.discord', { path: c.path });
    if (r.action === 'copied') {
      toast(discordReadyText(true));
      return;
    }
    discord.job = { key: c.key, id: r.id, pass: 1, pct: 0, reason: r.reason };
    toast(`Making a Discord copy: ${r.reason}…`);
  } catch (e) {
    toast(`Couldn't copy it for Discord: ${message(e)}`, 'error');
  }
}

export async function cancelDiscord(): Promise<void> {
  const job = discord.job;
  if (!job) return;
  try {
    await call('export.cancel', { id: job.id });
  } catch {
    // Already finished: its export_done clears the job.
  }
}

function discordEvent(e: KlipEvent): void {
  const job = discord.job;
  if (!job) return;
  if (e.event === 'export_progress' && e.id === job.id) {
    discord.job = { ...job, pass: e.pass, pct: progressPct(e.frames_done, e.frames_total) };
  } else if (e.event === 'export_done' && e.id === job.id) {
    discord.job = null;
    if (!e.ok) {
      if (e.error === 'cancelled') toast('Discord copy cancelled');
      else toast(`The Discord copy failed: ${e.error ?? 'unknown error'}`, 'error');
    } else if (e.copied) {
      toast(discordReadyText(false));
    } else {
      toast(`The Discord copy is saved but couldn't be copied: ${e.copy_error ?? 'unknown error'}`, 'error');
    }
  }
}
