// The settings as the engine sends them (klip-settings schema v1) and the pure helpers
// the Settings page and first-run setup use. Unit-tested in settings.test.ts.

import type { Appearance } from './appearance';

export type Preset = 'low' | 'balanced' | 'high' | 'custom';
export type VideoCodec = 'h264' | 'hevc' | 'av1';
export type HotkeyAction = 'clip.save' | 'session.toggle' | 'bookmark.add' | 'overlay.menu';

export interface HotkeyBinding {
  action: HotkeyAction;
  keys: string;
  secs: number;
}

export interface Quality {
  preset: Preset;
  custom: { max_width: number; max_height: number; fps: number; bitrate_kbps: number; codec: VideoCodec };
}

export interface Profile {
  buffer_secs: number | null;
  quality: Quality | null;
  desktop_audio: 'game' | 'system' | 'off' | null;
  voice: boolean | null;
  mic: boolean | null;
  auto_clip: boolean | null;
}

export interface Settings {
  version: number;
  app: {
    first_run_done: boolean;
    autostart: boolean;
    start_minimized: boolean;
    close_to_tray: boolean;
    control_channel: boolean;
    update_check: boolean;
  };
  clips: { folder: string | null; per_game_folders: boolean; recordings_folder: string | null };
  recording: { buffer_secs: number; quality: Quality; show_cursor: boolean; encoder: string | null };
  audio: {
    desktop: 'game' | 'system' | 'off';
    voice: boolean;
    voice_apps: string[];
    mic: boolean;
    mic_device: string | null;
    exclude_apps: string[];
    default_excludes: boolean;
    separate_tracks: boolean;
    bitrate_kbps: number;
  };
  hotkeys: HotkeyBinding[];
  overlay: {
    mode: 'off' | 'minimal' | 'full';
    sound: boolean;
    notification: boolean;
    corner: 'top-left' | 'top-right' | 'bottom-left' | 'bottom-right';
    seconds: number;
    recording_dot: boolean;
  };
  storage: { cap_gb: number; over_cap: 'trash' | 'delete'; low_disk_gb: number };
  export: { discord_limit_mb: number };
  appearance: Appearance;
  games: {
    buffer: 'games' | 'always' | 'off';
    detect_stores: boolean;
    user_games: { exe: string; name: string | null }[];
    never: string[];
    profiles: Record<string, Profile>;
  };
}

/** Same ranges as klip-settings. */
export const ranges = {
  buffer_secs: [5, 1800],
  clip_secs: [5, 3600],
  overlay_seconds: [1, 10],
  max_width: [320, 7680],
  max_height: [240, 4320],
  fps: [10, 240],
  bitrate_kbps: [1000, 200_000],
  cap_gb: [0, 1_000_000],
  low_disk_gb: [0, 100_000],
  discord_limit_mb: [1, 1000],
} as const;

export const audioBitrates = [96, 128, 160, 192, 256, 320];

export const presets: { id: Preset; label: string; detail: string }[] = [
  { id: 'low', label: 'Low', detail: '720p · 30 fps · 8 Mbit/s' },
  { id: 'balanced', label: 'Balanced', detail: '1080p · 60 fps · 20 Mbit/s' },
  { id: 'high', label: 'High', detail: 'up to 1440p · your refresh rate up to 144 fps · 40 Mbit/s' },
  { id: 'custom', label: 'Custom', detail: 'Choose size, frame rate, bitrate and codec' },
];

export const bufferChoices = [30, 60, 120, 300, 600, 1200, 1800];

/** `['recording', 'buffer_secs'], 60` → `{recording: {buffer_secs: 60}}` (a merge patch). */
export function patchFor(path: string[], value: unknown): Record<string, unknown> {
  let patch: unknown = value;
  for (const key of [...path].reverse()) patch = { [key]: patch };
  return patch as Record<string, unknown>;
}

/** The engine's rejection `"recording.buffer_secs: 2 is outside 5–1800"` → path + text. */
export function splitError(message: string): { path: string; text: string } {
  const m = /^([a-z_]+(?:\.[a-z_0-9-]+|\[\d+\])*): (.*)$/is.exec(message);
  return m ? { path: m[1] ?? '', text: m[2] ?? message } : { path: '', text: message };
}

// --- Hotkey capture ------------------------------------------------------------------

const codeNames: Record<string, string> = {
  Space: 'Space',
  Tab: 'Tab',
  Enter: 'Enter',
  NumpadEnter: 'Enter',
  Backspace: 'Backspace',
  Insert: 'Insert',
  Delete: 'Delete',
  Home: 'Home',
  End: 'End',
  PageUp: 'PageUp',
  PageDown: 'PageDown',
  ArrowUp: 'Up',
  ArrowDown: 'Down',
  ArrowLeft: 'Left',
  ArrowRight: 'Right',
  PrintScreen: 'PrintScreen',
  Pause: 'Pause',
  ScrollLock: 'ScrollLock',
  Backquote: '`',
  Minus: '-',
  Equal: '=',
  BracketLeft: '[',
  BracketRight: ']',
  Backslash: '\\',
  Semicolon: ';',
  Quote: "'",
  Comma: ',',
  Period: '.',
  Slash: '/',
  NumpadAdd: 'NumAdd',
  NumpadSubtract: 'NumSubtract',
  NumpadMultiply: 'NumMultiply',
  NumpadDivide: 'NumDivide',
  NumpadDecimal: 'NumDecimal',
};

const modifierCodes = new Set([
  'ControlLeft',
  'ControlRight',
  'AltLeft',
  'AltRight',
  'ShiftLeft',
  'ShiftRight',
  'MetaLeft',
  'MetaRight',
  'OSLeft',
  'OSRight',
]);

/** Our key name for `KeyboardEvent.code` (physical key), or null if we don't support it. */
export function keyName(code: string): string | null {
  let m: RegExpExecArray | null;
  if ((m = /^Key([A-Z])$/.exec(code))) return m[1] ?? null;
  if ((m = /^Digit(\d)$/.exec(code))) return m[1] ?? null;
  if ((m = /^F(\d{1,2})$/.exec(code))) {
    const n = Number(m[1]);
    return n >= 1 && n <= 24 ? `F${n}` : null;
  }
  if ((m = /^Numpad(\d)$/.exec(code))) return `Num${m[1]}`;
  return codeNames[code] ?? null;
}

export interface KeyLike {
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

export type Capture =
  | { kind: 'combo'; keys: string; bare: boolean }
  | { kind: 'cancel' }
  | { kind: 'clear' }
  | { kind: 'wait' }
  | { kind: 'unsupported'; code: string };

/** What a key press means while a hotkey field listens. Modifiers alone wait for a
    key; Esc cancels; Backspace or Delete alone clears the hotkey. */
export function captureKey(e: KeyLike): Capture {
  if (modifierCodes.has(e.code)) return { kind: 'wait' };
  const mods = e.ctrlKey || e.altKey || e.shiftKey || e.metaKey;
  if (e.code === 'Escape' && !mods) return { kind: 'cancel' };
  if ((e.code === 'Backspace' || e.code === 'Delete') && !mods) return { kind: 'clear' };
  const key = keyName(e.code);
  if (!key) return { kind: 'unsupported', code: e.code };
  const parts = [
    e.ctrlKey ? 'Ctrl' : null,
    e.altKey ? 'Alt' : null,
    e.shiftKey ? 'Shift' : null,
    e.metaKey ? 'Win' : null,
    key,
  ].filter(Boolean);
  return { kind: 'combo', keys: parts.join('+'), bare: !mods };
}

/** `Alt+Shift+F10` → `['Alt', 'Shift', 'F10']` (a `+` key doesn't exist). */
export function keyParts(keys: string): string[] {
  return keys ? keys.split('+') : [];
}

/** Hotkey state the engine reports for a binding's keys (e.g. taken by another app). */
export function bindingError(keys: string, bound: { keys: string | null; error: string | null }[]): string | null {
  if (!keys) return null;
  return bound.find((b) => b.keys === keys)?.error ?? null;
}
