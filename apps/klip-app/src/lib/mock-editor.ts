/**
 * The mock engine's editor and export commands (`npm run dev`): projects in memory,
 * repaired roughly like the engine (clip length wins, speeds and volumes clamped),
 * text bitmaps as SVG data URLs, exports that tick `export_progress` and end with
 * `export_done` (the "Discord" preset takes two passes, "youtube" fails once in a while
 * so the dialog's error path can be seen).
 */

import { CommandError } from './api';
import type { LibClip } from './library';
import type { Project, Speed } from './timeline';
import { asProject, newProject, outputDurationMs } from './timeline';
import type { KlipEvent } from './types';

type Args = Record<string, unknown> | null | undefined;

const PRESETS = ['original', 'youtube', 'tiktok', 'discord'];

function repair(raw: unknown, durationMs: number): { project: Project; issues: { path: string; message: string }[] } {
  const p = asProject(raw);
  const issues: { path: string; message: string }[] = [];
  p.version = 1;
  p.source_ms = durationMs;
  p.pieces = p.pieces
    .filter((x) => typeof x?.start_ms === 'number' && typeof x?.end_ms === 'number')
    .map((x) => ({
      start_ms: Math.max(0, Math.min(durationMs, Math.trunc(x.start_ms))),
      end_ms: Math.max(0, Math.min(durationMs, Math.trunc(x.end_ms))),
      speed: (x.speed === 0.5 || x.speed === 2 ? x.speed : 1) as Speed,
    }))
    .filter((x) => x.end_ms > x.start_ms)
    .sort((a, b) => a.start_ms - b.start_ms);
  const clamp = (v: unknown, path: string) => {
    const n = typeof v === 'number' && Number.isFinite(v) ? v : 1;
    const c = Math.min(2, Math.max(0, n));
    if (c !== n) issues.push({ path, message: `clamped to ${c}` });
    return c;
  };
  p.volumes = { game: clamp(p.volumes?.game, 'volumes.game'), mic: clamp(p.volumes?.mic, 'volumes.mic') };
  if (p.format.kind !== 'tall') p.format.kind = 'wide';
  p.format.crop_x = Math.min(1, Math.max(0, Number(p.format.crop_x) || 0.5));
  const duration = outputDurationMs(p);
  p.text = p.text
    .filter((t) => typeof t?.text === 'string' && t.text.trim() !== '')
    .map((t, i) => ({
      id: typeof t.id === 'number' ? t.id : i + 1,
      preset: ['bold', 'pill', 'caption', 'title'].includes(t.preset) ? t.preset : 'bold',
      text: t.text.trim().slice(0, 200),
      position: ['top', 'middle', 'bottom'].includes(t.position) ? t.position : 'middle',
      start_ms: Math.max(0, Math.min(duration, Math.trunc(t.start_ms || 0))),
      end_ms: Math.max(0, Math.min(duration, Math.trunc(t.end_ms || 0))),
    }))
    .filter((t) => t.end_ms > t.start_ms);
  return { project: p, issues };
}

function textSvg(text: string, preset: string, width: number, height: number): { x: number; y: number; w: number; h: number; url: string } {
  const size = Math.round(height * (preset === 'title' ? 0.12 : preset === 'bold' ? 0.09 : 0.06));
  const upper = preset === 'caption' || preset === 'title';
  const shown = (upper ? text.toUpperCase() : text).replace(/[<&>]/g, ' ');
  const fill = preset === 'caption' ? '#ffd60a' : '#fff';
  const w = Math.min(width, Math.round(shown.length * size * 0.62 + size));
  const h = Math.round(size * 1.4);
  const y = preset === 'title' ? Math.round(height * 0.06) : preset === 'bold' ? Math.round((height - h) / 2) : Math.round(height * 0.94 - h);
  const x = Math.round((width - w) / 2);
  const bg = preset === 'pill' || preset === 'title' ? `<rect x="0" y="0" width="${w}" height="${h}" rx="${h / 2}" fill="rgba(0,0,0,.7)"/>` : '';
  const stroke = preset === 'bold' || preset === 'caption' ? `stroke="#000" stroke-width="${Math.max(2, size * 0.12)}" paint-order="stroke"` : '';
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}">${bg}<text x="${w / 2}" y="${h * 0.72}" text-anchor="middle" font-family="Inter, Segoe UI, sans-serif" font-weight="800" font-size="${size}" fill="${fill}" ${stroke}>${shown}</text></svg>`;
  return { x, y, w, h, url: `data:image/svg+xml;utf8,${encodeURIComponent(svg)}` };
}

export function mockEditor(emit: (e: KlipEvent) => void, byPath: (path: string) => LibClip | undefined) {
  const projects = new Map<string, Project>();
  let nextId = 0;
  let active: { id: number; key: string; preset: string; out: string; timer: ReturnType<typeof setInterval> } | null = null;
  let last: unknown = null;

  const clipOf = (args: Args): LibClip => {
    const path = args?.path;
    if (typeof path !== 'string') throw new CommandError('bad_args', 'path must be a string');
    const clip = byPath(path);
    if (!clip) throw new CommandError('bad_args', 'no such clip');
    return clip;
  };
  const sourceOf = (clip: LibClip) => ({
    width: clip.info?.width ?? 1920,
    height: clip.info?.height ?? 1080,
    duration_ms: clip.info?.duration_ms ?? 30_000,
    fps: clip.info?.fps ?? 60,
    bookmarks_ms: clip.info?.bookmarks_ms ?? [],
  });
  const reply = (clip: LibClip, project: Project, issues: unknown[], status: string) => ({
    key: clip.key,
    path: clip.path,
    project,
    issues,
    status,
    source: sourceOf(clip),
  });

  return (cmd: string, args: Args): unknown => {
    switch (cmd) {
      case 'editor.project.get': {
        const clip = clipOf(args);
        const saved = projects.get(clip.path);
        const src = sourceOf(clip);
        if (!saved) return reply(clip, newProject(src.duration_ms), [], 'new');
        return reply(clip, saved, [], 'saved');
      }
      case 'editor.project.set': {
        const clip = clipOf(args);
        const raw = args?.project;
        if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw new CommandError('bad_args', 'project must be an object');
        const { project, issues } = repair(raw, sourceOf(clip).duration_ms);
        projects.set(clip.path, project);
        return reply(clip, project, issues, 'saved');
      }
      case 'editor.proxy': {
        const clip = clipOf(args);
        // The browser can't play a path; the preview falls back to the strip picture.
        return { status: 'original', path: clip.path, key: clip.key };
      }
      case 'editor.text.render': {
        const clip = clipOf(args);
        const layer = args?.layer as { text?: string; preset?: string } | undefined;
        const width = Number(args?.width) || 1920;
        const height = Number(args?.height) || 1080;
        if (!layer || typeof layer.text !== 'string') throw new CommandError('bad_args', 'layer must be a text layer');
        const r = textSvg(layer.text, layer.preset ?? 'bold', width, height);
        void clip;
        return { x: r.x, y: r.y, width: r.w, height: r.h, frame_width: width, frame_height: height, png: null, url: r.url };
      }
      case 'export.start': {
        const clip = clipOf(args);
        const preset = args?.preset;
        if (typeof preset !== 'string' || !PRESETS.includes(preset)) throw new CommandError('bad_args', 'unknown preset');
        if (active) throw new CommandError('busy', 'an export is already running');
        const id = ++nextId;
        const label = preset[0]!.toUpperCase() + preset.slice(1);
        const out = clip.path.replace(/\.mp4$/i, ` - ${label === 'Youtube' ? 'YouTube' : label === 'Tiktok' ? 'TikTok' : label}.mp4`);
        const project = projects.get(clip.path) ?? newProject(sourceOf(clip).duration_ms);
        const total = Math.round((outputDurationMs(project) / 1000) * 60);
        let pass = 1;
        let done = 0;
        const timer = setInterval(() => {
          done += Math.max(1, Math.round(total / 40));
          if (done >= total) {
            if (preset === 'discord' && pass === 1) {
              pass = 2;
              done = 0;
            } else {
              clearInterval(timer);
              active = null;
              const ok = !(preset === 'youtube' && id % 3 === 0);
              last = {
                id,
                key: clip.key,
                preset,
                ok,
                path: ok ? out : null,
                error: ok ? null : 'encoding failed: nvd3d11h264enc: the encoder stopped taking data',
                summary: ok ? { bytes: 8_400_000, duration_ms: outputDurationMs(project), width: 1920, height: 1080, fps: 60, passes: pass } : null,
              };
              emit(last as KlipEvent & { event: 'export_done' });
              if (ok) emit({ event: 'library_changed', reason: 'exported' });
              return;
            }
          }
          emit({ event: 'export_progress', id, key: clip.key, preset, pass, frames_done: Math.min(done, total), frames_total: total });
        }, 120);
        active = { id, key: clip.key, preset, out, timer };
        return { id, out, preset };
      }
      case 'export.cancel': {
        const id = args?.id;
        if (!active || active.id !== id) throw new CommandError('not_found', 'no such export');
        clearInterval(active.timer);
        const a = active;
        active = null;
        last = { id: a.id, key: a.key, preset: a.preset, ok: false, path: null, error: 'cancelled', summary: null };
        setTimeout(() => emit(last as KlipEvent & { event: 'export_done' }), 60);
        return { id };
      }
      case 'export.status':
        return {
          active: active ? { id: active.id, key: active.key, out: active.out, preset: active.preset, pass: 1, frames_done: 0, frames_total: 0, elapsed_ms: 0 } : null,
          proxy: null,
          last,
        };
      default:
        return undefined;
    }
  };
}
