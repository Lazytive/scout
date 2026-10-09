// A stand-in engine for `npm run dev` in a plain browser (never part of the app build:
// api.ts only imports it in development outside Tauri). It plays a short story: no game,
// then a game starts and the replay buffer runs; commands answer like the real engine.

import { CommandError } from './api';
import { mockLibrary } from './mock-library';
import type { Settings } from './settings';
import type { Game, KlipEvent, Status } from './types';

const game: Game = {
  pid: 4242,
  exe: 'eldenring.exe',
  key: 'eldenring.exe',
  name: 'ELDEN RING',
  window: 1234,
  source: { kind: 'library', store: 'steam' },
  foreground: true,
};

export function mockBackend() {
  const listeners = new Set<(e: KlipEvent) => void>();
  const emit = (e: KlipEvent) => listeners.forEach((l) => l(e));
  const started = Date.now();
  const status: Status = {
    buffer_mode: 'games',
    game: null,
    recording: false,
    desktop: false,
    starting: false,
    clip_dir: null,
    session: null,
    recorder: null,
    last_error: null,
    hotkeys: [
      { id: 'clip.save.0', action: 'clip.save', keys: 'Alt+F10', secs: 30, trigger: 'ALT+F10', error: null },
      { id: 'clip.save.1', action: 'clip.save', keys: 'Alt+Shift+F10', secs: 120, trigger: 'ALT+SHIFT+F10', error: null },
      { id: 'session.toggle.0', action: 'session.toggle', keys: 'Alt+F9', secs: 0, trigger: 'ALT+F9', error: 'already used by another app' },
      { id: 'bookmark.add.0', action: 'bookmark.add', keys: 'Alt+F8', secs: 0, trigger: 'ALT+F8', error: null },
    ],
    settings_status: 'loaded',
    settings_issues: [],
  };

  setTimeout(() => {
    status.game = game;
    status.recording = true;
    status.session = false;
    status.clip_dir = 'C:\\Users\\you\\Videos\\Klip\\ELDEN RING';
    status.recorder = { encoder: 'nvd3d11h264enc', width: 1920, height: 1080, fps: 60, buffer_ms: 120_000 };
    emit({ event: 'game_started', game });
    emit({
      event: 'recorder_started',
      game: game.name,
      encoder: 'nvd3d11h264enc',
      width: 1920,
      height: 1080,
      fps: 60,
      buffer_ms: 120_000,
      clip_dir: status.clip_dir,
    });
  }, 1500);

  // The engine's defaults (klip-settings), so every settings page can be worked on.
  let settings: Settings = {
    version: 1,
    app: { first_run_done: !new URLSearchParams(location.search).has('firstrun'), autostart: false, start_minimized: true, close_to_tray: true, control_channel: true },
    clips: { folder: null, per_game_folders: true, recordings_folder: null },
    recording: {
      buffer_secs: 120,
      quality: { preset: 'balanced', custom: { max_width: 1920, max_height: 1080, fps: 60, bitrate_kbps: 20000, codec: 'h264' } },
      show_cursor: true,
      encoder: null,
    },
    audio: {
      desktop: 'game',
      voice: true,
      voice_apps: ['Discord.exe', 'DiscordPTB.exe', 'TeamSpeak.exe'],
      mic: false,
      mic_device: null,
      exclude_apps: [],
      default_excludes: true,
      separate_tracks: true,
      bitrate_kbps: 160,
    },
    hotkeys: [
      { action: 'clip.save', keys: 'Alt+F10', secs: 30 },
      { action: 'clip.save', keys: 'Alt+Shift+F10', secs: 120 },
      { action: 'session.toggle', keys: 'Alt+F9', secs: 0 },
      { action: 'bookmark.add', keys: 'Alt+F8', secs: 0 },
      { action: 'overlay.menu', keys: '', secs: 0 },
    ],
    overlay: { mode: 'minimal', sound: true, notification: false, corner: 'top-right', seconds: 3, recording_dot: true },
    storage: { cap_gb: 50, over_cap: 'trash', low_disk_gb: 10 },
    appearance: { theme: 'system' },
    games: { buffer: 'games', detect_stores: true, user_games: [{ exe: 'MyIndieGame.exe', name: 'My Indie Game' }], never: [], profiles: {} },
  };

  /** RFC 7396 merge patch. */
  const merge = (target: unknown, patch: unknown): unknown => {
    if (!patch || typeof patch !== 'object' || Array.isArray(patch)) return patch;
    const out: Record<string, unknown> =
      target && typeof target === 'object' && !Array.isArray(target) ? { ...(target as Record<string, unknown>) } : {};
    for (const [k, v] of Object.entries(patch)) {
      if (v === null) delete out[k];
      else out[k] = merge(out[k], v);
    }
    return out;
  };
  let meterTimer: ReturnType<typeof setInterval> | null = null;
  const library = mockLibrary(emit);

  const need = () => {
    if (!status.recording) throw new CommandError('not_ready', 'no game is running');
  };

  return {
    async call(cmd: string, args: unknown): Promise<unknown> {
      await new Promise((r) => setTimeout(r, 120));
      switch (cmd) {
        case 'engine.status': {
          if (status.recorder) {
            const up = Date.now() - started - 1500;
            status.recorder.buffered_ms = Math.max(0, Math.min(120_000, up));
            status.recorder.bytes = (status.recorder.buffered_ms / 1000) * 2_600_000;
          }
          return structuredClone(status);
        }
        case 'settings.get':
          return { settings: structuredClone(settings) };
        case 'settings.set': {
          const next = merge(settings, args) as Settings;
          if (next.recording.buffer_secs < 5 || next.recording.buffer_secs > 1800) {
            throw new CommandError('bad_args', `recording.buffer_secs: ${next.recording.buffer_secs} is outside 5–1800`);
          }
          const keys = next.hotkeys.map((h) => h.keys).filter(Boolean);
          const dup = keys.findIndex((k, i) => keys.indexOf(k) !== i);
          if (dup >= 0) {
            const i = next.hotkeys.findIndex((h, j) => h.keys === keys[dup] && j > next.hotkeys.findIndex((x) => x.keys === keys[dup]));
            throw new CommandError('bad_args', `hotkeys[${i}].keys: ${keys[dup]} is already used by another action`);
          }
          settings = next;
          emit({ event: 'settings_changed', settings: structuredClone(settings) });
          return { settings: structuredClone(settings) };
        }
        case 'recording.estimate': {
          const d = (args ?? settings) as Settings;
          const kbps = { low: 8000, balanced: 20000, high: 40000, custom: d.recording.quality.custom.bitrate_kbps }[d.recording.quality.preset];
          const audio = d.audio.bitrate_kbps * (d.audio.mic ? 3 : 2);
          return { bytes: ((kbps + audio) * 1000 / 8) * 1.5 * (d.recording.buffer_secs + 1), video_kbps: kbps, audio_kbps: audio, fps: 60 };
        }
        case 'encoders.list':
          await new Promise((r) => setTimeout(r, 900));
          return {
            gpus: [{ name: 'NVIDIA GeForce RTX 4080 SUPER', vendor: 'NVIDIA' }],
            codec: 'h264',
            encoders: [
              { element: 'nvd3d11h264enc', description: 'NVENC H.264 (Direct3D11)', gpu: 'NVIDIA' },
              { element: 'nvcudah264enc', description: 'NVENC H.264 (CUDA)', gpu: 'NVIDIA' },
              { element: 'mfh264enc', description: 'Media Foundation H.264', gpu: 'NVIDIA' },
            ],
            chosen: 'nvd3d11h264enc',
          };
        case 'audio.devices':
          return {
            inputs: [
              { id: 'mic-1', name: 'Microphone (USB Audio)' },
              { id: 'mic-2', name: 'Headset Microphone' },
            ],
            outputs: [],
          };
        case 'audio.meter': {
          if (meterTimer) clearInterval(meterTimer);
          meterTimer = null;
          if ((args as { on?: boolean }).on) {
            meterTimer = setInterval(() => {
              const v = Math.random();
              emit({ event: 'mic_level', peak_db: -30 + v * 26, rms_db: -42 + v * 22 });
            }, 66);
          }
          return { on: !!meterTimer };
        }
        case 'hotkeys.pause':
          return { paused: true };
        case 'hotkeys.resume':
          return { paused: false };
        case 'games.candidates':
          return {
            programs: [
              { exe: 'eldenring.exe', name: 'ELDEN RING', detected: true },
              { exe: 'Blender.exe', name: 'Blender', detected: false },
              { exe: 'RetroArch.exe', name: 'RetroArch', detected: false },
            ],
          };
        case 'storage.status':
          return {
            folder: 'C:\\Users\\you\\Videos\\Klip',
            cap_bytes: 50 * 1024 ** 3,
            over_cap: 'trash',
            usage: { clips: 214, bytes: 31.4 * 1024 ** 3, starred: 12, starred_bytes: 2.1 * 1024 ** 3 },
            free_bytes: 412 * 1024 ** 3,
            low_disk: false,
          };
        case 'clip.save': {
          need();
          const last = (args as { last_ms?: number } | null)?.last_ms ?? 30_000;
          const clip = { path: 'C:\\clips\\Klip_2026-10-09.mp4', duration_ms: last, bytes: last * 2600, frames: last * 0.06, save_ms: 9, bookmarks: 0 };
          emit({ event: 'clip_saved', game: game.name, window: game.window, clip });
          return clip;
        }
        case 'session.toggle': {
          need();
          status.session = !status.session;
          if (status.session) emit({ event: 'recording_started', path: 'C:\\clips\\rec.mp4' });
          else emit({ event: 'recording_stopped', summary: {} });
          return { recording: status.session };
        }
        case 'bookmark.add':
          need();
          emit({ event: 'bookmark', result: { recording_ms: status.session ? 4000 : null } });
          return {};
        default: {
          const r = library(cmd, args);
          if (r !== undefined) return r;
          throw new CommandError('unknown_command', `mock: ${cmd}`);
        }
      }
    },
    async listen(cb: (e: KlipEvent) => void) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
  };
}
