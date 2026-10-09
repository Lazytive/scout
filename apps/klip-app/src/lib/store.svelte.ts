// App state: the engine's status (refreshed on events and every few seconds while the
// window is open) and a short activity log.

import { call, onEvent, setWindowTheme } from './api';
import { describeEvent, type Tone } from './format';
import { applyTheme, resolve, type ThemeSetting } from './themes';
import type { KlipEvent, Status, StorageStatus } from './types';

export interface Activity {
  id: number;
  at: Date;
  text: string;
  tone: Tone;
}

const MAX_ACTIVITY = 50;

export const app = $state({
  status: null as Status | null,
  storage: null as StorageStatus | null,
  /** The engine's settings (`settings.get`), kept fresh from `settings_changed`. */
  settings: null as Record<string, unknown> | null,
  themeSetting: 'dark' as ThemeSetting,
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
  app.settings = s as Record<string, unknown>;
  const appearance = app.settings.appearance as { theme?: ThemeSetting } | undefined;
  app.themeSetting = appearance?.theme ?? 'dark';
  showTheme();
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
