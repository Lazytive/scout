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

/** Subscribe to engine events; returns the unsubscribe function. */
export async function onEvent(cb: (e: KlipEvent) => void): Promise<() => void> {
  return (await getBackend()).listen(cb);
}
