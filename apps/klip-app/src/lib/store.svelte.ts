// App state: the engine's status (refreshed on events and every few seconds while the
// window is open) and a short activity log.

import {
  applyLook,
  contrastProblems,
  defaultAppearance,
  resolveLook,
  samplePicture,
  type Appearance,
  type ContrastProblem,
  type CustomTheme,
  type Look,
} from './appearance';
import { CommandError, call, onEvent, pickThemeExport, pickThemeFile, pictureUrl, setWindowTheme } from './api';
import { describeEvent, type Tone } from './format';
import { patchFor, splitError, type Settings } from './settings';
import { editorEvent } from './editor.svelte';
import { libraryEvent } from './library.svelte';
import type { ThemeSetting } from './themes';
import type { KlipEvent, Status, StorageStatus } from './types';

export interface Activity {
  id: number;
  at: Date;
  text: string;
  tone: Tone;
}

const MAX_ACTIVITY = 50;

/** The Library is the start page (M11.2: the old Home folded into it). */
export type Page = 'library' | 'editor' | 'settings';

export const app = $state({
  /** The workspace shown. */
  page: 'library' as Page,
  /** The Settings category shown (`''`: the first). */
  settingsPage: '',
  status: null as Status | null,
  storage: null as StorageStatus | null,
  /** The engine's settings (`settings.get`), kept fresh from `settings_changed`. */
  settings: null as Settings | null,
  /** Rejected changes by setting path (`recording.buffer_secs`), until the next save. */
  fieldErrors: {} as Record<string, string>,
  /** Microphone meter (`audio.meter`), dBFS. */
  mic: { peak: -100, rms: -100 },
  /** `appearance.theme`, or `custom:<id>` for a custom theme. */
  themeSetting: 'system' as ThemeSetting | `custom:${string}`,
  /** Custom themes (`theme.list`) and what was wrong with their files. */
  customThemes: [] as CustomTheme[],
  themeIssues: [] as { id: string; path: string; message: string }[],
  /** The theme editor's draft, shown instead of the custom theme in use. */
  themePreview: null as CustomTheme | null,
  /** The look on screen and its contrast problems (the warning in Appearance). */
  look: null as Look | null,
  contrast: [] as ContrastProblem[],
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

const lightQuery = typeof window !== 'undefined' ? window.matchMedia('(prefers-color-scheme: light)') : null;
const motionQuery = typeof window !== 'undefined' ? window.matchMedia('(prefers-reduced-motion: reduce)') : null;

/** The settings' appearance, with defaults for anything missing. */
export function appearance(): Appearance {
  return { ...defaultAppearance, ...(app.settings?.appearance ?? {}) };
}

let contrastRun = 0;

/** Apply the look the settings ask for (System follows the OS) and check its contrast. */
export function showTheme(): void {
  const look = resolveLook(
    app.settings?.appearance,
    app.customThemes,
    { prefersLight: lightQuery?.matches ?? false, prefersReducedMotion: motionQuery?.matches ?? false },
    pictureUrl,
    app.themePreview,
  );
  applyLook(look);
  app.look = look;
  void setWindowTheme(look.scheme);
  const run = ++contrastRun;
  const bg = look.background;
  void (bg ? samplePicture(bg.url, look.scheme, bg.dim) : Promise.resolve(null)).then((pic) => {
    if (run === contrastRun) app.contrast = contrastProblems(look, pic);
  });
}

function useSettings(s: unknown) {
  if (!s || typeof s !== 'object') return;
  app.settings = s as Settings;
  const a = appearance();
  app.themeSetting = a.theme === 'custom' ? `custom:${a.custom_theme ?? ''}` : a.theme;
  showTheme();
}

/** Reload the custom themes (`theme.list`). */
export async function refreshThemes(): Promise<void> {
  try {
    const r = await call<{ themes: CustomTheme[]; issues: { id: string; path: string; message: string }[] }>(
      'theme.list',
    );
    app.customThemes = r.themes;
    app.themeIssues = r.issues;
  } catch {
    app.customThemes = [];
  }
  showTheme();
}

/** The editor's form of a theme (what `theme.save` takes). */
function themeForm(t: CustomTheme) {
  return {
    name: t.name,
    base: t.base,
    colors: t.colors,
    radius: t.radius,
    font: t.font ?? null,
    background: t.background ? { dim: t.background.dim, opacity: t.background.opacity } : null,
  };
}

async function themeCall<T>(cmd: string, args: unknown): Promise<T | null> {
  app.error = null;
  try {
    return await call<T>(cmd, args);
  } catch (e) {
    app.error = e instanceof Error ? e.message : String(e);
    return null;
  }
}

/** A new custom theme from `from` (a copy) or a built-in, saved and put in use. */
export async function createTheme(
  from: CustomTheme | { base: CustomTheme['base']; name: string },
): Promise<CustomTheme | null> {
  const t: CustomTheme =
    'id' in from
      ? { ...structuredClone($state.snapshot(from) as CustomTheme), name: `${from.name} copy`.slice(0, 48) }
      : { id: '', name: from.name, base: from.base, colors: {}, radius: 6, font: null, background: null };
  const r = await themeCall<{ id: string; theme: CustomTheme }>('theme.save', { theme: themeForm(t) });
  if (!r) return null;
  // Keep the picture of the theme copied from.
  let theme = r.theme;
  if ('id' in from && from.background) {
    const p = await themeCall<{ theme: CustomTheme }>('theme.save', {
      id: r.id,
      theme: themeForm(t),
      picture: { file: from.background.path },
    });
    if (p) theme = p.theme;
  }
  await refreshThemes();
  await setTheme(`custom:${r.id}`);
  return theme;
}

/** Save the editor's draft; `picture`: a new file, `null` to remove, absent to keep. */
export async function saveTheme(t: CustomTheme, picture?: string | null): Promise<CustomTheme | null> {
  const args: Record<string, unknown> = { id: t.id, theme: themeForm(t) };
  if (picture !== undefined) args.picture = picture === null ? null : { file: picture };
  const r = await themeCall<{ theme: CustomTheme }>('theme.save', args);
  if (r) await refreshThemes();
  return r?.theme ?? null;
}

/** Import a theme file the user picks and put it in use; returns what was repaired. */
export async function importTheme(): Promise<{ name: string; issues: { path: string; message: string }[] } | null> {
  const path = await pickThemeFile();
  if (!path) return null;
  const r = await themeCall<{ id: string; theme: CustomTheme; issues: { path: string; message: string }[] }>(
    'theme.import',
    { path },
  );
  if (!r) return null;
  await refreshThemes();
  await setTheme(`custom:${r.id}`);
  return { name: r.theme.name, issues: r.issues };
}

/** Export a custom theme to a file the user picks; returns the path. */
export async function exportTheme(t: CustomTheme): Promise<string | null> {
  const path = await pickThemeExport(t.name);
  if (!path) return null;
  const r = await themeCall<{ path: string }>('theme.export', { id: t.id, path });
  return r?.path ?? null;
}

export async function deleteTheme(id: string): Promise<boolean> {
  const r = await themeCall<{ deleted: boolean }>('theme.delete', { id });
  await refreshThemes();
  return !!r;
}

/** Change one appearance setting: shown at once, then saved (a rejection is shown next
    to the field and the saved settings come back). */
export async function setAppearance<K extends keyof Appearance>(key: K, value: Appearance[K]): Promise<boolean> {
  if (app.settings) {
    app.settings = { ...app.settings, appearance: { ...appearance(), [key]: value } };
    showTheme();
  }
  const ok = await save(['appearance', key], value);
  if (!ok) await refreshSettings();
  return ok;
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
    const { path: at, text } =
      e instanceof CommandError && e.code === 'bad_args' ? splitError(message) : { path: '', text: message };
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

/** Change the theme (applied at once, saved in the settings): a built-in, Follow
    system, or `custom:<id>`. */
export async function setTheme(id: ThemeSetting | `custom:${string}`): Promise<void> {
  app.themeSetting = id;
  const patch = id.startsWith('custom:')
    ? { theme: 'custom', custom_theme: id.slice('custom:'.length) }
    : { theme: id };
  if (app.settings) {
    app.settings = { ...app.settings, appearance: { ...appearance(), ...patch } as Appearance };
  }
  showTheme();
  await run('settings.set', { appearance: patch });
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
    if (e.event === 'themes_changed') void refreshThemes();
    void refresh();
    if (e.event === 'clip_saved' || e.event === 'storage_cleaned' || e.event === 'settings_changed') {
      void refreshStorage();
    }
  });
  const onScheme = () => showTheme();
  lightQuery?.addEventListener('change', onScheme);
  motionQuery?.addEventListener('change', onScheme);
  await Promise.all([refreshSettings(), refresh(), refreshStorage(), refreshThemes()]);
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
    motionQuery?.removeEventListener('change', onScheme);
  };
}
