// A stand-in engine for `npm run dev` in a plain browser (never part of the app build:
// api.ts only imports it in development outside Tauri). It plays a short story: no game,
// then a game starts and the replay buffer runs; commands answer like the real engine.

import { CommandError } from './api';
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

  const settings = { appearance: { theme: 'system' as string }, overlay: { mode: 'minimal' } };

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
          const theme = (args as { appearance?: { theme?: string } } | null)?.appearance?.theme;
          if (theme) settings.appearance.theme = theme;
          emit({ event: 'settings_changed', settings: structuredClone(settings) });
          return { settings: structuredClone(settings) };
        }
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
        default:
          throw new CommandError('unknown_command', `mock: ${cmd}`);
      }
    },
    async listen(cb: (e: KlipEvent) => void) {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
  };
}
