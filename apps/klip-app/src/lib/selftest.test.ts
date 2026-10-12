import { describe, expect, it } from 'vitest';

import { runSelfTest, type SelfTestDeps } from './selftest';
import type { KlipEvent } from './types';

/** A small engine: AMF works, Media Foundation fails, recording and exports succeed. */
function fakeEngine() {
  const settings = { games: { buffer: 'games' }, recording: { encoder: null as string | null, quality: 'balanced' } };
  let recorder: string | null = null;
  const listeners: ((e: KlipEvent) => void)[] = [];
  const emit = (e: KlipEvent) => setTimeout(() => listeners.splice(0).forEach((l) => l(e)), 1);
  const clips: {
    key: string;
    path: string;
    name: string;
    bytes: number;
    thumbs: { poster: number | null; strip: number | null };
    info: unknown;
  }[] = [];
  const deleted: string[] = [];
  let exportId = 0;
  let last: unknown = null;
  let t = 0;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- a fake engine's loose JSON
  const call = async (cmd: string, args?: any): Promise<any> => {
    switch (cmd) {
      case 'engine.status':
        return { game: null, recorder, last_error: null };
      case 'settings.get':
        return { settings: JSON.parse(JSON.stringify(settings)) };
      case 'audio.devices':
        return { inputs: [{}], outputs: [{}, {}] };
      case 'encoders.list': {
        expect(args.fresh).toBe(true);
        if (args.codec === 'h264') {
          return {
            gpus: [{ name: 'AMD Radeon', vendor: 'AMD' }],
            tested: [
              { element: 'amfh264enc', ok: true, probe_ms: 2000 },
              { element: 'mfh264enc', ok: false, error: 'no device' },
            ],
            chosen: 'amfh264enc',
            took_ms: 3000,
          };
        }
        return {
          gpus: [],
          tested: [{ element: `amf${args.codec}enc`, ok: args.codec === 'h265', probe_ms: 1 }],
          chosen: null,
          took_ms: 1,
        };
      }
      case 'settings.set': {
        if (args.games?.buffer) settings.games.buffer = args.games.buffer;
        if (args.recording && 'encoder' in args.recording) settings.recording.encoder = args.recording.encoder;
        if (settings.games.buffer === 'off') recorder = null;
        else if (settings.games.buffer === 'always' && !recorder) {
          recorder = settings.recording.encoder;
          emit({
            event: 'recorder_started',
            game: null,
            encoder: recorder,
            width: 2560,
            height: 1440,
            fps: 144,
            buffer_ms: 120000,
            clip_dir: 'C:/v',
          });
        }
        return {};
      }
      case 'clip.save': {
        const path = `C:/v/Desktop/Scout_${clips.length}.mp4`;
        clips.push({
          key: `Desktop/${clips.length}`,
          path,
          name: 'x',
          bytes: 5e6,
          thumbs: { poster: null, strip: null },
          info: { codec: 'H.264', width: 2560, height: 1440, fps: 144, duration_ms: 4000, audio_tracks: 3 },
        });
        emit({
          event: 'clip_saved',
          game: null,
          window: null,
          clip: { path, duration_ms: 4000, bytes: 5e6, frames: 576, save_ms: 80, bookmarks: 0 },
        });
        return {};
      }
      case 'library.list':
        return { clips };
      case 'library.thumbs':
        for (const c of clips) c.thumbs = { poster: 1, strip: 1 };
        return {};
      case 'editor.frame':
        return { width: 640, height: 360, jpeg: 'abc' };
      case 'export.start':
        exportId += 1;
        last = {
          id: exportId,
          ok: true,
          error: null,
          path: `C:/v/Desktop/out-${args.preset}.mp4`,
          summary: { width: 1920, height: 1080, bytes: 3e6, passes: args.preset === 'discord' ? 2 : 1 },
        };
        return { id: exportId, out: '' };
      case 'export.status':
        return { active: null, last };
      case 'clip.delete':
        deleted.push(...args.paths);
        return { removed: args.paths.length };
      default:
        throw new Error(`unexpected ${cmd}`);
    }
  };
  const deps: SelfTestDeps = {
    call,
    waitFor: (pred, ms) =>
      new Promise((resolve) => {
        const l = (e: KlipEvent) => (pred(e) ? resolve(e) : listeners.push(l));
        listeners.push(l);
        setTimeout(() => resolve(null), ms);
      }),
    sleep: async () => undefined,
    now: () => (t += 100),
    info: { version: '0.0.1', full_logs: true, bundled_gstreamer: true },
  };
  return { deps, settings, deleted };
}

describe('the Debug self-test', () => {
  it('runs every step, then deletes its clips and puts the settings back', async () => {
    const { deps, settings, deleted } = fakeEngine();
    const seen: string[] = [];
    const r = await runSelfTest(deps, (steps) => seen.push(steps.map((s) => s.status).join(',')));
    const status = Object.fromEntries(r.steps.map((s) => [s.id, s.status]));
    expect(status).toEqual({
      system: 'ok',
      'encoders-h264': 'ok',
      'encoders-h265': 'ok',
      'encoders-av1': 'warn',
      'record-amfh264enc': 'ok',
      thumbs: 'ok',
      frame: 'ok',
      'export-youtube': 'ok',
      'export-discord': 'ok',
      cleanup: 'ok',
    });
    expect(r.failed).toBe(0);
    expect(settings).toEqual({ games: { buffer: 'games' }, recording: { encoder: null, quality: 'balanced' } });
    expect(deleted).toEqual([
      'C:/v/Desktop/Scout_0.mp4',
      'C:/v/Desktop/out-youtube.mp4',
      'C:/v/Desktop/out-discord.mp4',
    ]);
    expect(r.report).toContain('[OK  ] Recording with amfh264enc');
    expect(r.report).toContain('mfh264enc: fails (no device)');
    expect(r.report).toContain('recorded 2560×1440 at 144 fps, encoder amfh264enc, screen');
    expect(seen.length).toBeGreaterThan(10);
  });
});
