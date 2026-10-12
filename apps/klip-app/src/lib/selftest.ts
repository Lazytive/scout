// The Debug page's self-test (tester builds; the user's AMD tester, 2026-10-12): Scout's
// real paths one after another — fresh encoder probes for every codec, a real recording
// with each working H.264 encoder (the screen, or the game when one runs), its thumbnails
// and a preview frame, YouTube and Discord exports — then the test clips are deleted and
// the settings put back. Every step's outcome goes into a plain-text report.
//
// The engine is reached through `SelfTestDeps`, so a fake engine can drive it in tests.

import type { KlipEvent } from './types';

export type StepStatus = 'pending' | 'running' | 'ok' | 'warn' | 'fail' | 'skip';

export interface Step {
  id: string;
  label: string;
  status: StepStatus;
  detail: string;
}

export interface SelfTestDeps {
  call: <T = unknown>(cmd: string, args?: unknown) => Promise<T>;
  /** The first event `pred` accepts within `ms`, else null. */
  waitFor: (pred: (e: KlipEvent) => boolean, ms: number) => Promise<KlipEvent | null>;
  sleep: (ms: number) => Promise<void>;
  now: () => number;
  info: { version: string; full_logs: boolean; bundled_gstreamer: boolean };
}

interface Tested {
  element: string;
  ok: boolean | null;
  error?: string | null;
  probe_ms?: number;
}

interface EncodersResult {
  gpus: { name: string; vendor: string; vram_bytes?: number }[];
  tested?: Tested[];
  chosen: string | null;
  error?: string;
  took_ms: number;
}

interface LibClip {
  key: string;
  path: string;
  name: string;
  bytes: number;
  thumbs: { poster: number | null; strip: number | null };
  info: {
    codec: string | null;
    width: number;
    height: number;
    fps: number | null;
    duration_ms: number | null;
    audio_tracks: number;
  } | null;
}

interface ExportEnd {
  id: number;
  ok: boolean;
  error: string | null;
  path: string | null;
  summary: Record<string, number> | null;
}

/** Most H.264 encoders recorded with (each takes ~15 s). */
export const MAX_RECORD_ENCODERS = 4;
const CODECS: [string, string][] = [
  ['h264', 'H.264'],
  ['h265', 'HEVC'],
  ['av1', 'AV1'],
];

function message(e: unknown): string {
  if (e && typeof e === 'object' && 'message' in e) return String((e as { message: unknown }).message);
  return String(e);
}

function mb(bytes: number): string {
  return `${(bytes / 1_000_000).toFixed(1)} MB`;
}

/** The report's text. */
export function reportText(steps: Step[], info: SelfTestDeps['info'], startedIso: string, ms: number): string {
  const icon: Record<StepStatus, string> = {
    pending: '…',
    running: '…',
    ok: 'OK  ',
    warn: 'WARN',
    fail: 'FAIL',
    skip: 'SKIP',
  };
  const lines = [
    `Scout self-test, ${startedIso}`,
    `Scout ${info.version}, full logs ${info.full_logs ? 'on' : 'off'}, bundled GStreamer ${info.bundled_gstreamer ? 'yes' : 'no'}`,
    `Took ${(ms / 1000).toFixed(0)} s. ${steps.filter((s) => s.status === 'fail').length} failed, ${steps.filter((s) => s.status === 'warn').length} warnings.`,
    '',
  ];
  for (const s of steps) {
    lines.push(`[${icon[s.status]}] ${s.label}`);
    for (const d of s.detail.split('\n').filter(Boolean)) lines.push(`       ${d}`);
  }
  return lines.join('\n') + '\n';
}

/** Run every step; `onChange` gets the steps after each change. */
export async function runSelfTest(
  deps: SelfTestDeps,
  onChange: (steps: Step[]) => void,
): Promise<{ steps: Step[]; report: string; failed: number }> {
  const started = deps.now();
  const steps: Step[] = [];
  const add = (id: string, label: string): Step => {
    const s: Step = { id, label, status: 'pending', detail: '' };
    steps.push(s);
    return s;
  };
  const set = (s: Step, status: StepStatus, detail = s.detail) => {
    s.status = status;
    s.detail = detail;
    onChange(steps.map((x) => ({ ...x })));
  };

  const system = add('system', 'System');
  const enc = CODECS.map(([id, name]) => add(`encoders-${id}`, `${name} encoders (fresh probe)`));
  onChange(steps.map((x) => ({ ...x })));

  // 1. System.
  set(system, 'running');
  let original: { buffer: string; encoder: string | null } | null = null;
  try {
    const st = await deps.call<Record<string, unknown>>('engine.status');
    const s = await deps.call<{
      settings: { games: { buffer: string }; recording: { encoder: string | null; quality: unknown } };
    }>('settings.get');
    original = { buffer: s.settings.games.buffer, encoder: s.settings.recording.encoder };
    let audio = '';
    try {
      const a = await deps.call<{ outputs?: unknown[]; inputs?: unknown[] }>('audio.devices');
      audio = `audio devices: ${a.outputs?.length ?? '?'} out, ${a.inputs?.length ?? '?'} in`;
    } catch (e) {
      audio = `audio devices: ${message(e)}`;
    }
    const game = (st.game as { name?: string } | null)?.name;
    set(
      system,
      'ok',
      [
        `buffer mode ${original.buffer}, encoder ${original.encoder ?? 'automatic'}, quality ${JSON.stringify(s.settings.recording.quality)}`,
        game
          ? `game running: ${game} (the recording test records it)`
          : 'no game running (the recording test records the screen)',
        st.last_error ? `last recorder error: ${String(st.last_error)}` : '',
        audio,
      ]
        .filter(Boolean)
        .join('\n'),
    );
  } catch (e) {
    set(system, 'fail', message(e));
  }

  // 2. Encoders, probed again.
  const h264Ok: string[] = [];
  for (const [i, [codec]] of CODECS.entries()) {
    const step = enc[i];
    if (!step) continue;
    set(step, 'running');
    try {
      const r = await deps.call<EncodersResult>('encoders.list', { codec, fresh: true });
      const tested = r.tested ?? [];
      const lines = tested.map((t) =>
        t.ok
          ? `${t.element}: works (${t.probe_ms ?? '?'} ms)`
          : `${t.element}: ${t.ok === null ? 'not tested' : 'fails'}${t.error ? ` (${t.error})` : ''}`,
      );
      if (i === 0) lines.unshift(`GPUs: ${r.gpus.map((g) => `${g.name} (${g.vendor})`).join(', ') || 'none found'}`);
      lines.push(`chosen: ${r.chosen ?? 'none'}${r.error ? ` (${r.error})` : ''}; ${r.took_ms} ms`);
      const working = tested.filter((t) => t.ok).map((t) => t.element);
      if (codec === 'h264') h264Ok.push(...working);
      set(step, working.length > 0 ? 'ok' : codec === 'h264' ? 'fail' : 'warn', lines.join('\n'));
    } catch (e) {
      set(step, codec === 'h264' ? 'fail' : 'warn', message(e));
    }
  }

  // 3. A real recording with each working H.264 encoder.
  const testClips: string[] = [];
  let firstClip: LibClip | null = null;
  const toRecord = h264Ok.slice(0, MAX_RECORD_ENCODERS);
  const recSteps = toRecord.map((el) => add(`record-${el}`, `Recording with ${el}`));
  const thumbs = add('thumbs', 'Thumbnails of the test clip');
  const frame = add('frame', 'Editor preview frame');
  const yt = add('export-youtube', 'Export: YouTube preset');
  const dc = add('export-discord', 'Export: Discord preset');
  const cleanup = add('cleanup', 'Clean up (test clips deleted, settings put back)');
  onChange(steps.map((x) => ({ ...x })));
  if (toRecord.length === 0) {
    for (const s of [thumbs, frame, yt, dc]) set(s, 'skip', 'no working H.264 encoder');
  }

  const stopRecorder = async () => {
    await deps.call('settings.set', { games: { buffer: 'off' } });
    for (let i = 0; i < 40; i++) {
      const st = await deps.call<{ recorder: unknown }>('engine.status');
      if (!st.recorder) return;
      await deps.sleep(250);
    }
  };

  for (const [i, el] of toRecord.entries()) {
    const step = recSteps[i];
    if (!step) continue;
    set(step, 'running');
    try {
      await stopRecorder();
      const startedEvent = deps.waitFor((e) => e.event === 'recorder_started' || e.event === 'recorder_failed', 30_000);
      await deps.call('settings.set', { recording: { encoder: el }, games: { buffer: 'always' } });
      const ev = await startedEvent;
      if (!ev) throw new Error('the recorder did not start within 30 s');
      if (ev.event === 'recorder_failed') throw new Error(`the recorder failed: ${ev.error}`);
      if (ev.event !== 'recorder_started') throw new Error('unexpected event');
      const shape = `${ev.width ?? '?'}×${ev.height ?? '?'} at ${ev.fps} fps, encoder ${ev.encoder ?? '?'}${ev.game ? `, game ${ev.game}` : ', screen'}`;
      await deps.sleep(5000);
      const savedEvent = deps.waitFor((e) => e.event === 'clip_saved', 15_000);
      await deps.call('clip.save', { last_ms: 4000 });
      const saved = await savedEvent;
      if (!saved || saved.event !== 'clip_saved') throw new Error(`recording ${shape}: no clip saved within 15 s`);
      testClips.push(saved.clip.path);
      let clip: LibClip | null = null;
      for (let t = 0; t < 20 && !clip?.info; t++) {
        const l = await deps.call<{ clips: LibClip[] }>('library.list');
        clip = l.clips.find((c) => c.path === saved.clip.path) ?? null;
        if (!clip?.info) await deps.sleep(250);
      }
      const inf = clip?.info;
      const lines = [
        `recorded ${shape}`,
        `saved ${saved.clip.frames} frames, ${(saved.clip.duration_ms / 1000).toFixed(1)} s, ${mb(saved.clip.bytes)} in ${saved.clip.save_ms.toFixed(0)} ms`,
        inf
          ? `file: ${inf.codec ?? '?'} ${inf.width}×${inf.height}, ${inf.fps?.toFixed(1) ?? '?'} fps, ${((inf.duration_ms ?? 0) / 1000).toFixed(1)} s, ${inf.audio_tracks} audio track(s)`
          : 'the library could not read the file',
      ];
      const good = !!inf && (inf.duration_ms ?? 0) >= 2500 && saved.clip.frames > 0;
      if (good && !firstClip && clip) firstClip = clip;
      if (ev.encoder && ev.encoder !== el) lines.push(`note: the recorder used ${ev.encoder}, not ${el}`);
      set(step, good ? (inf && inf.audio_tracks === 0 ? 'warn' : 'ok') : 'fail', lines.join('\n'));
    } catch (e) {
      set(step, 'fail', message(e));
    }
  }

  // 4. Thumbnails, a preview frame and exports of the first good clip.
  const exportPaths: string[] = [];
  if (firstClip) {
    const c = firstClip;
    set(thumbs, 'running');
    try {
      await deps.call('library.thumbs', { keys: [c.key] });
      let ready = false;
      for (let t = 0; t < 80 && !ready; t++) {
        const l = await deps.call<{ clips: LibClip[] }>('library.list');
        const x = l.clips.find((y) => y.key === c.key);
        ready = !!x?.thumbs.poster && !!x?.thumbs.strip;
        if (!ready) await deps.sleep(250);
      }
      set(thumbs, ready ? 'ok' : 'fail', ready ? 'poster and hover strip made' : 'no thumbnails within 20 s');
    } catch (e) {
      set(thumbs, 'fail', message(e));
    }
    set(frame, 'running');
    try {
      const t0 = deps.now();
      const f = await deps.call<{ width: number; height: number; jpeg?: string }>('editor.frame', {
        path: c.path,
        out_ms: 1000,
        width: 640,
      });
      set(frame, f.jpeg ? 'ok' : 'fail', `${f.width}×${f.height} in ${deps.now() - t0} ms`);
    } catch (e) {
      set(frame, 'fail', message(e));
    }
    for (const [step, preset] of [
      [yt, 'youtube'],
      [dc, 'discord'],
    ] as const) {
      set(step, 'running');
      try {
        const t0 = deps.now();
        const s = await deps.call<{ id: number; out: string }>('export.start', { path: c.path, preset });
        let last: ExportEnd | null = null;
        for (let t = 0; t < 600; t++) {
          const x = await deps.call<{ active: unknown; last: ExportEnd | null }>('export.status');
          if (!x.active && x.last?.id === s.id) {
            last = x.last;
            break;
          }
          await deps.sleep(250);
        }
        if (!last) throw new Error('the export did not finish within 150 s');
        if (last.path) exportPaths.push(last.path);
        const sm = last.summary;
        set(
          step,
          last.ok ? 'ok' : 'fail',
          last.ok
            ? `${sm?.width ?? '?'}×${sm?.height ?? '?'}, ${mb(sm?.bytes ?? 0)}, ${sm?.passes ?? 1} pass(es), ${((deps.now() - t0) / 1000).toFixed(1)} s`
            : (last.error ?? 'failed'),
        );
      } catch (e) {
        set(step, 'fail', message(e));
      }
    }
  } else if (toRecord.length > 0) {
    for (const s of [thumbs, frame, yt, dc]) set(s, 'skip', 'no good test recording');
  }

  // 5. Put things back.
  set(cleanup, 'running');
  const problems: string[] = [];
  try {
    if (original) {
      if (toRecord.length > 0) await stopRecorder();
      await deps.call('settings.set', { recording: { encoder: original.encoder }, games: { buffer: original.buffer } });
    }
  } catch (e) {
    problems.push(`settings: ${message(e)}`);
  }
  const doomed = [...testClips, ...exportPaths];
  if (doomed.length > 0) {
    try {
      await deps.call('clip.delete', { paths: doomed, permanent: true });
    } catch (e) {
      problems.push(`deleting the test clips: ${message(e)}`);
    }
  }
  set(
    cleanup,
    problems.length > 0 ? 'warn' : 'ok',
    problems.length > 0 ? problems.join('\n') : `${doomed.length} test file(s) deleted; settings as before`,
  );

  const ms = deps.now() - started;
  const report = reportText(steps, deps.info, new Date(started).toISOString(), ms);
  return { steps, report, failed: steps.filter((s) => s.status === 'fail').length };
}
