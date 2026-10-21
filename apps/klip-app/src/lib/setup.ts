// Scout's setup window (ADR 0023): what it says in each phase, and its two backends:
// the setup app over Tauri, and a pretend one for `npm run dev` at /setup.html
// (`?update`, `?passive` and `?fail` try the other paths).

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

import { formatBytes } from './format';

export type SetupPhase = 'ready' | 'installing' | 'done' | 'failed';

/** `setup_info`: what the setup app knows. */
export interface SetupInfo {
  version: string;
  dir: string;
  /** `/P`: install without a click (the updater's way). */
  passive: boolean;
  /** `/R`: open Scout when done. */
  run_after: boolean;
  /** `/UPDATE`: over an installed Scout. */
  update: boolean;
  /** A development build of the setup carries no installer. */
  has_installer: boolean;
  expected_bytes: number;
}

export function setupTitle(phase: SetupPhase, info: SetupInfo | null): string {
  const v = info?.version ?? '';
  const update = info?.update ?? false;
  switch (phase) {
    case 'installing':
      return update ? `Updating Scout to ${v}…` : `Installing Scout ${v}…`;
    case 'done':
      return update ? `Scout ${v} is ready` : 'Scout is installed';
    case 'failed':
      return update ? 'Scout could not be updated' : 'Scout could not be installed';
    default:
      return (update ? `Update to Scout ${v}` : `Install Scout ${v}`).trim();
  }
}

export function setupLine(phase: SetupPhase, info: SetupInfo | null, pct: number, error: string | null): string {
  switch (phase) {
    case 'installing':
      return `${pct} % · ${info?.dir ?? ''}`.trim();
    case 'done':
      return 'Open it now, or later from the Start menu.';
    case 'failed':
      return error ?? 'unknown error';
    default:
      if (!info) return '';
      if (!info.has_installer) return 'This build of the setup carries no installer.';
      return `For you only, in ${info.dir} · about ${formatBytes(info.expected_bytes)}.`;
  }
}

export function primaryLabel(phase: SetupPhase): string {
  return { ready: 'Install', installing: 'Installing…', done: 'Open Scout', failed: 'Try again' }[phase];
}

export function secondaryLabel(phase: SetupPhase): string | null {
  return { ready: 'Cancel', installing: null, done: 'Close', failed: 'Close' }[phase];
}

export interface SetupBackend {
  info(): Promise<SetupInfo>;
  /** Resolves when the installer is done; rejects with why it failed. */
  install(onProgress: (pct: number) => void): Promise<void>;
  open(): Promise<void>;
  quit(): Promise<void>;
}

const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

export function setupBackend(): SetupBackend {
  return inTauri ? tauriBackend() : mockBackend();
}

function tauriBackend(): SetupBackend {
  return {
    info: () => invoke<SetupInfo>('setup_info'),
    install: (onProgress) =>
      new Promise<void>((resolve, reject) => {
        void (async () => {
          const stopProgress = await listen<{ pct: number }>('setup://progress', (e) => onProgress(e.payload.pct));
          const stopDone = await listen<{ ok: boolean; error?: string }>('setup://done', (e) => {
            stopProgress();
            stopDone();
            if (e.payload.ok) resolve();
            else reject(new Error(e.payload.error ?? 'the installer failed'));
          });
          try {
            await invoke('setup_install');
          } catch (err) {
            stopProgress();
            stopDone();
            reject(err instanceof Error ? err : new Error(String(err)));
          }
        })();
      }),
    open: () => invoke('setup_open'),
    quit: () => invoke('setup_quit'),
  };
}

function mockBackend(): SetupBackend {
  const q = typeof window !== 'undefined' ? new URLSearchParams(window.location.search) : new URLSearchParams();
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  return {
    info: async () => ({
      version: '0.1.0',
      dir: 'C:\\Users\\you\\AppData\\Local\\Scout',
      passive: q.has('passive'),
      run_after: q.has('passive'),
      update: q.has('update'),
      has_installer: !q.has('empty'),
      expected_bytes: 58_000_000,
    }),
    install: async (onProgress) => {
      for (let p = 0; p <= 95; p += 5) {
        await sleep(120);
        onProgress(p);
      }
      if (q.has('fail')) throw new Error('the installer ended with exit code: 2 (pretend)');
    },
    open: async () => {},
    quit: async () => {},
  };
}
