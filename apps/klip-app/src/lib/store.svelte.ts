// App state: the engine's status (refreshed on events and every few seconds while the
// window is open) and a short activity log.

import { CommandError, call, onEvent, setWindowTheme } from './api';
import { describeEvent, type Tone } from './format';
import { patchFor, splitError, type Settings } from './settings';
import { editorEvent } from './editor.svelte';
import { libraryEvent } from './library.svelte';
import { applyTheme, resolve, type ThemeSetting } from './themes';
import type { KlipEvent, Status, StorageStatus } from './types';

export interface Activity {
  id: number;
  at: Date;
  text: string;
  tone: Tone;
}

const MAX_ACTIVITY = 50;

export type Page = 'home' | 'library' | 'editor' | 'settings';

export const app = $state({
  /** The workspace shown. */
  page: 'home' as Page,
  status: null as Status | null,
  storage: null as StorageStatus | null,
  /** The engine's settings (`settings.get`), kept fresh from `settings_changed`. */
  settings: null as Settings | null,
  /** Rejected changes by setting path (`recording.buffer_secs`), until the next save. */
  fieldErrors: {} as Record<string, string>,
  /** Microphone meter (`audio.meter`), dBFS. */
  mic: { peak: -100, rms: -100 },
  themeSetting: 'system' as ThemeSetting,
  activity: [] as Activity[],
  /** The last command error, shown until the next command. */
  error: null as string | null,
  busy: false,
});

let nextId = 0;

function log(e: KlipEvent) {
  const d = describeEvent(e);
  if (!d) return;
  nextId += 1;
  app.activity = [{ id: nextId, at: new Date(), ...d }, ...app.activity].slice(0, MAX_ACTIVITY);
}

export async function refresh(): Promise<void> {
  try {
    app.status = await call<Status>('engine.status');
  } catch (e) {
    app.error = e instanceof Error ? e.message : String(e);
  }
}

const lightQuery =
  typeof window !== 'undefined' ? window.matchMedia('(prefers-color-scheme: light)') : null;

/** Apply the theme the settings ask for (System follows the OS). */
function showTheme() {
  const theme = resolve(app.themeSetting, lightQuery?.matches ?? false);
  applyTheme(theme);
  void setWindowTheme(theme.scheme);
}

function useSettings(s: unknown) {
  if (!s || typeof s !== 'object') return;
  app.settings = s as Settings;
  app.themeSetting = (app.settings.appearance?.theme as ThemeSetting | undefined) ?? 'system';
  showTheme();
}

/** Save one setting (a strict merge patch). A rejection is shown next to the field. */
export async function save(path: string[], value: unknown): Promise<boolean> {
  const key = path.join('.');
  try {
    const r = await call<{ settings: unknown }>('settings.set', patchFor(path, value));
    useSettings(r.settings);
    const errors = { ...app.fieldErrors };
    for (const k of Object.keys(errors)) {
      if (k === key || k.startsWith(`${key}.`) || k.startsWith(`${key}[`)) delete errors[k];
    }
    app.fieldErrors = errors;
    return true;
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    const { path: at, text } = e instanceof CommandError && e.code === 'bad_args' ? splitError(message) : { path: '', text: message };
    app.fieldErrors = { ...app.fieldErrors, [at || key]: text };
    return false;
  }
}

/** The error shown for a setting path (or anything below it). */
export function fieldError(path: string): string | null {
  for (const [k, v] of Object.entries(app.fieldErrors)) {
    if (k === path || k.startsWith(`${path}.`) || k.startsWith(`${path}[`)) return v;
  }
  return null;
}

/** Microphone meter on or off. */
export async function meter(on: boolean, device: string | null = null): Promise<void> {
  try {
    await call('audio.meter', on ? { on: true, device } : { on: false });
  } catch (e) {
    app.error = e instanceof Error ? e.message : String(e);
  }
  if (!on) app.mic = { peak: -100, rms: -100 };
}

/** While a hotkey field listens, the engine's hotkeys are off (or the combination
    would run instead of reaching the window). */
export async function pauseHotkeys(): Promise<void> {
  await call('hotkeys.pause').catch(() => undefined);
}

export async function resumeHotkeys(): Promise<void> {
  await call('hotkeys.resume').catch(() => undefined);
}

export async function refreshSettings(): Promise<void> {
  try {
    const r = await call<{ settings: unknown }>('settings.get');
    useSettings(r.settings);
  } catch (e) {
    app.error = e instanceof Error ? e.message : String(e);
  }
}

/** Change the theme (applied at once, saved in the settings). */
export async function setTheme(id: ThemeSetting): Promise<void> {
  app.themeSetting = id;
  showTheme();
  await run('settings.set', { appearance: { theme: id } });
}

export async function refreshStorage(): Promise<void> {
  try {
    app.storage = await call<StorageStatus>('storage.status');
  } catch {
    app.storage = null;
  }
}

/** Run a command for a button; errors end up in `app.error`. */
export async function run(cmd: string, args?: unknown): Promise<void> {
  app.busy = true;
  app.error = null;
  try {
    await call(cmd, args);
  } catch (e) {
    app.error = e instanceof Error ? e.message : String(e);
  } finally {
    app.busy = false;
    await refresh();
  }
}

/** Start listening; returns a stop function. */
export async function connect(): Promise<() => void> {
  const unlisten = await onEvent((e) => {
    if (e.event === 'mic_level') {
      app.mic = { peak: e.peak_db, rms: e.rms_db };
      return;
    }
    libraryEvent(e);
    editorEvent(e);
    if (e.event === 'thumbs_ready' || e.event === 'library_changed' || e.event === 'export_progress') return;
    log(e);
    if (e.event === 'settings_changed') useSettings(e.settings);
    void refresh();
    if (e.event === 'clip_saved' || e.event === 'storage_cleaned' || e.event === 'settings_changed') {
      void refreshStorage();
    }
  });
  const onScheme = () => showTheme();
  lightQuery?.addEventListener('change', onScheme);
  await Promise.all([refreshSettings(), refresh(), refreshStorage()]);
  let ticks = 0;
  const timer = setInterval(() => {
    if (document.visibilityState !== 'visible') return;
    ticks += 1;
    void refresh();
    if (ticks % 10 === 0) void refreshStorage();
  }, 2000);
  return () => {
    unlisten();
    clearInterval(timer);
    lightQuery?.removeEventListener('change', onScheme);
  };
}
