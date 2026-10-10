// The one door to the engine: every action is a command (`klip_control::commands`), and
// the engine's events arrive on `klip://event`. In a plain browser (`npm run dev`) a
// mock backend stands in, so the UI can be worked on without the app.

import type { KlipEvent } from './types';

export class CommandError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.code = code;
  }
}

interface Backend {
  call(cmd: string, args: unknown): Promise<unknown>;
  listen(cb: (e: KlipEvent) => void): Promise<() => void>;
}

const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

async function tauriBackend(): Promise<Backend> {
  const { invoke } = await import('@tauri-apps/api/core');
  const { listen } = await import('@tauri-apps/api/event');
  return {
    async call(cmd, args) {
      try {
        return await invoke('klip', { cmd, args: args ?? null });
      } catch (e) {
        const err = (e ?? {}) as { code?: string; message?: string };
        throw new CommandError(err.code ?? 'failed', err.message ?? String(e));
      }
    },
    listen: (cb) => listen<KlipEvent>('klip://event', (e) => cb(e.payload)),
  };
}

let backend: Promise<Backend> | null = null;

function getBackend(): Promise<Backend> {
  if (!backend) {
    backend =
      inTauri || !import.meta.env.DEV
        ? tauriBackend()
        : import('./mock').then((m) => m.mockBackend());
  }
  return backend;
}

/** Run a command; rejects with a `CommandError` (`code`, `message`). */
export async function call<T = unknown>(cmd: string, args?: unknown): Promise<T> {
  return (await getBackend()).call(cmd, args) as Promise<T>;
}

/** Match the native title bar to the theme (no-op outside the app). */
export async function setWindowTheme(scheme: 'light' | 'dark'): Promise<void> {
  if (!inTauri) return;
  try {
    const { getCurrentWindow } = await import('@tauri-apps/api/window');
    await getCurrentWindow().setTheme(scheme);
  } catch {
    // The window keeps the OS theme; nothing else depends on it.
  }
}

/** Ask for a folder (the system's picker in the app, a prompt in a browser). */
export async function pickFolder(current: string | null): Promise<string | null> {
  if (!inTauri) return window.prompt('Folder', current ?? '') || null;
  const { open } = await import('@tauri-apps/plugin-dialog');
  const picked = await open({ directory: true, multiple: false, defaultPath: current ?? undefined });
  return typeof picked === 'string' ? picked : null;
}

export type ThumbKind = 'poster' | 'strip';

let thumbProvider: ((kind: ThumbKind, key: string, v: number) => string) | null = null;

/** The mock engine draws its own thumbnails. */
export function setThumbProvider(p: (kind: ThumbKind, key: string, v: number) => string): void {
  thumbProvider = p;
}

// The app's `klipthumb` scheme; WebView2 (Windows) spells custom schemes as http://<scheme>.localhost.
const thumbBase =
  typeof navigator !== 'undefined' && /Windows/i.test(navigator.userAgent)
    ? 'http://klipthumb.localhost/'
    : 'klipthumb://localhost/';

/** URL of a clip's cached thumbnail (`v`: its version, so a new one isn't cached). */
export function thumbUrl(kind: ThumbKind, key: string, v: number): string {
  if (thumbProvider) return thumbProvider(kind, key, v);
  return `${thumbBase}${kind}/${encodeURIComponent(key)}?v=${v}`;
}

/** Subscribe to engine events; returns the unsubscribe function. */
export async function onEvent(cb: (e: KlipEvent) => void): Promise<() => void> {
  return (await getBackend()).listen(cb);
}

export interface EditorMedia {
  status: 'original' | 'ready' | 'making';
  path?: string;
  id?: number;
  key: string;
  /** A URL the page can play (Tauri's asset protocol); absent in the mock. */
  url?: string;
}

async function viaTauri<T>(path: string, what: 'proxy' | 'audio' | 'files'): Promise<T> {
  const core = await import('@tauri-apps/api/core');
  try {
    return await core.invoke<T>('editor_media', { path, what });
  } catch (e) {
    const err = e as { code?: string; message?: string };
    throw new CommandError(err?.code ?? 'failed', err?.message ?? String(e));
  }
}

/** A file the engine named, as a URL the page can load (Tauri's asset protocol). */
export async function fileUrl(path: string): Promise<string | null> {
  if (!inTauri) return null;
  const core = await import('@tauri-apps/api/core');
  return core.convertFileSrc(path);
}

/**
 * What the editor's preview plays (`editor.proxy`). In the app this goes through the
 * Tauri command `editor_media`, which allows exactly the files the engine names for
 * the asset protocol and so yields a playable URL; the mock has no file to play.
 */
export async function editorMedia(path: string): Promise<EditorMedia> {
  if (!inTauri) return call<EditorMedia>('editor.proxy', { path });
  const r = await viaTauri<EditorMedia>(path, 'proxy');
  if (r.path && r.status !== 'making') return { ...r, url: (await fileUrl(r.path)) ?? undefined };
  return r;
}

export interface AudioTrack {
  index: number;
  /** `Mix`, `Game`, `Microphone`, … (empty when the file doesn't say). */
  name: string;
  path: string;
  /** Playable URL (absent in the mock). */
  url?: string;
}

/** A clip's sound tracks one per file (`editor.audio`), each with a playable URL in the app. */
export async function editorAudio(path: string): Promise<AudioTrack[]> {
  const r = inTauri
    ? await viaTauri<{ tracks: AudioTrack[] }>(path, 'audio')
    : await call<{ tracks: AudioTrack[] }>('editor.audio', { path });
  const tracks = Array.isArray(r?.tracks) ? r.tracks : [];
  return Promise.all(tracks.map(async (t) => ({ ...t, url: (await fileUrl(t.path)) ?? undefined })));
}

export interface ProjectFile {
  path: string;
  kind: 'image' | 'audio';
  ok: boolean;
  error?: string;
  url?: string;
}

/** The project's pictures and sounds (`editor.files`), the usable ones with a URL. */
export async function editorFiles(path: string): Promise<ProjectFile[]> {
  const r = inTauri
    ? await viaTauri<{ files: ProjectFile[] }>(path, 'files')
    : await call<{ files: ProjectFile[] }>('editor.files', { path });
  const files = Array.isArray(r?.files) ? r.files : [];
  return Promise.all(files.map(async (f) => ({ ...f, url: f.ok ? ((await fileUrl(f.path)) ?? undefined) : undefined })));
}
