/**
 * The mock engine's editor and export commands (`npm run dev`): projects in memory,
 * repaired roughly like the engine (clip length wins, speeds and volumes clamped),
 * text bitmaps as SVG data URLs, exports that tick `export_progress` and end with
 * `export_done` (the "Discord" preset takes two passes, "youtube" fails once in a while
 * so the dialog's error path can be seen). Advanced mode (M9.4 commands): montages
 * (`editor.montage`, `clips` in the reply), `editor.frame` as an SVG picture of what is
 * on screen, history snapshots on every save, Custom exports; no audio or files to
 * play (`editor.audio` / `editor.files` answer with none).
 */

import { CommandError } from './api';
import { readAdvanced } from './advanced';
import { compose, visibleAt } from './compose';
import type { SourceInfo } from './editor';
import type { LibClip } from './library';
import type { Project, Speed } from './timeline';
import { asProject, newProject, outputDurationMs } from './timeline';
import type { KlipEvent } from './types';

type Args = Record<string, unknown> | null | undefined;

const PRESETS = ['original', 'youtube', 'tiktok', 'discord', 'custom'];
const CODECS = ['h264', 'hevc', 'av1'];
const MAX_SNAPSHOTS = 30;

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

export function mockEditor(emit: (e: KlipEvent) => void, allClips: () => LibClip[]) {
  const byPath = (path: string) => allClips().find((c) => c.path === path);
  const byKey = (key: string) => allClips().find((c) => c.key === key);
  const projects = new Map<string, Project>();
  const history = new Map<string, { id: number; project: Project }[]>();
  let lastSnapshot = 0;
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
  const sourceOf = (clip: LibClip): SourceInfo => ({
    width: clip.info?.width ?? 1920,
    height: clip.info?.height ?? 1080,
    duration_ms: clip.info?.duration_ms ?? 30_000,
    fps: clip.info?.fps ?? 60,
    bookmarks_ms: clip.info?.bookmarks_ms ?? [],
    events: [],
  });
  /** The other library clips a project places, as the engine lists them. */
  const placed = (project: Project) => {
    const keys = new Set<string>();
    for (const t of readAdvanced(project).tracks) for (const c of t.clips) if (c.source.kind === 'library') keys.add(c.source.key);
    return [...keys].map((key) => {
      const c = byKey(key);
      return c ? { key, name: c.name, status: 'ok', source: sourceOf(c), path: c.path } : { key, name: key, status: 'missing', source: null, path: null };
    });
  };
  const infos = (project: Project): [string, SourceInfo][] =>
    placed(project).flatMap((c) => (c.source ? [[c.key, c.source] as [string, SourceInfo]] : []));
  const reply = (clip: LibClip, project: Project, issues: unknown[], status: string) => ({
    key: clip.key,
    path: clip.path,
    project,
    issues,
    status,
    source: sourceOf(clip),
    clips: placed(project),
  });
  const snapshot = (clip: LibClip, previous: Project | undefined) => {
    if (!previous) return;
    const list = history.get(clip.key) ?? [];
    lastSnapshot = Math.max(Date.now(), lastSnapshot + 1);
    list.unshift({ id: lastSnapshot, project: previous });
    history.set(clip.key, list.slice(0, MAX_SNAPSHOTS));
  };
  /** What `editor.frame` shows: each visible layer's name on its own hue, as an SVG. */
  const frameSvg = (clip: LibClip, project: Project, outMs: number, width: number) => {
    const comp = compose(project, sourceOf(clip), infos(project));
    const height = Math.max(2, Math.round((width * comp.crop.height) / Math.max(1, comp.crop.width)) & ~1);
    const rows = visibleAt(comp, outMs).map((l, i) => {
      const s = l.source;
      const name = s.kind === 'library' ? (byKey(s.key)?.name ?? s.key) : s.kind === 'main' ? clip.name : s.path;
      const hue = (l.id * 67) % 360;
      const text = name.replace(/[<&>]/g, ' ');
      return (
        `<rect x="${i * 8}" y="${i * 8}" width="${width - i * 16}" height="${height - i * 16}" fill="hsl(${hue} 45% 30%)" opacity="0.8"/>` +
        `<text x="${width / 2}" y="${24 + i * 22}" fill="#fff" font-family="sans-serif" font-size="16" text-anchor="middle">${text}</text>`
      );
    });
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}"><rect width="100%" height="100%" fill="#000"/>${rows.join('')}</svg>`;
    return { url: `data:image/svg+xml;utf8,${encodeURIComponent(svg)}`, width, height };
  };

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
        const previous = projects.get(clip.path);
        if (JSON.stringify(previous) !== JSON.stringify(project)) snapshot(clip, previous);
        projects.set(clip.path, project);
        return reply(clip, project, issues, 'saved');
      }
      case 'editor.montage': {
        const clip = clipOf(args);
        const add = args?.add;
        if (!Array.isArray(add) || add.length === 0 || add.length > 256) throw new CommandError('bad_args', 'add must be a list of 1…256 clips');
        const project = projects.get(clip.path) ?? newProject(sourceOf(clip).duration_ms);
        const end = compose(project, sourceOf(clip), infos(project)).duration_ms;
        const next = asProject(JSON.parse(JSON.stringify(project)));
        const adv = readAdvanced(next);
        let track = adv.tracks.find((t) => t.kind === 'video');
        if (!track) {
          track = { id: adv.tracks.reduce((m, t) => Math.max(m, t.id), 0) + 1, kind: 'video', name: 'Montage', muted: false, hidden: false, main: false, clips: [] };
          adv.tracks.push(track);
        }
        let id = adv.tracks.reduce((m, t) => t.clips.reduce((n, c) => Math.max(n, c.id), m), 0) + 1;
        let at = end;
        for (const p of add) {
          const other = typeof p === 'string' ? byPath(p) : undefined;
          if (!other) throw new CommandError('bad_args', 'no such clip');
          const len = sourceOf(other).duration_ms;
          const source = other.key === clip.key ? { kind: 'main' as const } : { kind: 'library' as const, key: other.key };
          track.clips.push({ id: id++, source, in_ms: 0, out_ms: len, at_ms: at, transform: [], opacity: [], volume: [], speed: [] });
          at += len;
        }
        next.advanced = adv;
        snapshot(clip, project);
        projects.set(clip.path, next);
        return reply(clip, next, [], 'saved');
      }
      case 'editor.frame': {
        const clip = clipOf(args);
        const outMs = args?.out_ms;
        const width = args?.width;
        if (typeof outMs !== 'number') throw new CommandError('bad_args', 'out_ms must be a number');
        if (typeof width !== 'number' || width < 16 || width > 4096) throw new CommandError('bad_args', 'width must be 16…4096');
        const project = args?.project ? asProject(args.project) : (projects.get(clip.path) ?? newProject(sourceOf(clip).duration_ms));
        const f = frameSvg(clip, project, outMs, Math.trunc(width));
        return { jpeg: null, url: f.url, width: f.width, height: f.height, out_ms: outMs };
      }
      case 'editor.history.list': {
        const clip = clipOf(args);
        const list = history.get(clip.key) ?? [];
        return {
          key: clip.key,
          snapshots: list.map((h) => ({ id: h.id, saved_unix_ms: h.id, bytes: JSON.stringify(h.project).length })),
        };
      }
      case 'editor.history.restore': {
        const clip = clipOf(args);
        const list = history.get(clip.key) ?? [];
        const found = list.find((h) => h.id === args?.id);
        if (!found) throw new CommandError('not_found', 'no such snapshot');
        snapshot(clip, projects.get(clip.path));
        projects.set(clip.path, found.project);
        return reply(clip, found.project, [], 'saved');
      }
      case 'editor.audio': {
        const clip = clipOf(args);
        // The browser can't play a path: no tracks, the preview stays silent.
        return { status: 'ready', key: clip.key, tracks: [] };
      }
      case 'editor.files': {
        clipOf(args);
        return { files: [] };
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
        if (preset === 'custom') {
          const c = args?.custom as Record<string, unknown> | undefined;
          const num = (k: string, lo: number, hi: number) => {
            const v = c?.[k];
            if (typeof v !== 'number' || !Number.isInteger(v) || v < lo || v > hi) throw new CommandError('bad_args', `custom.${k} must be ${lo}…${hi}`);
          };
          num('width', 2, 4096);
          num('height', 2, 4096);
          num('fps', 1, 240);
          num('bitrate_kbps', 100, 200_000);
          if (typeof c?.codec !== 'string' || !CODECS.includes(c.codec)) throw new CommandError('bad_args', 'custom.codec must be h264, hevc or av1');
        }
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
