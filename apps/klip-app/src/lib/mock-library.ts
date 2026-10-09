// The mock engine's library (`npm run dev` only): ~60 made-up clips over five games and
// six weeks, drawn thumbnails (SVG), and the library commands answering like the real
// engine (filters and sorts done here the simple way).

import { CommandError, setThumbProvider, type ThumbKind } from './api';
import type { LibClip, Listing } from './library';
import type { KlipEvent } from './types';

const GAMES = ['ELDEN RING', 'Apex Legends', 'Counter-Strike 2', 'Rocket League', 'Hades II'];
const HUES = [18, 4, 205, 28, 280];
const TILES = 16;

function rng(seed: number) {
  let s = seed;
  return () => {
    s = (s * 1_103_515_245 + 12_345) % 2_147_483_648;
    return s / 2_147_483_648;
  };
}

function stamp(d: Date): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}_${p(d.getHours())}-${p(d.getMinutes())}-${p(d.getSeconds())}`;
}

function makeClips(): LibClip[] {
  const r = rng(7);
  const now = Date.now();
  const clips: LibClip[] = [];
  let t = now - 20 * 60_000;
  for (let i = 0; i < 62; i += 1) {
    const g = Math.floor(r() * GAMES.length);
    const game = GAMES[g] ?? null;
    const d = new Date(t);
    const recording = r() < 0.06;
    const duration = recording ? Math.round((8 + r() * 50) * 60_000) : r() < 0.7 ? 30_000 : 120_000;
    const name = recording ? `Klip_Recording_${stamp(d)}` : `Klip_${stamp(d)}`;
    const key = `${game}/${name}.mp4`;
    const ultrawide = g === 2 && r() < 0.5;
    clips.push({
      key,
      path: `C:\\Users\\you\\Videos\\Klip\\${game}\\${name}.mp4`,
      name,
      game,
      bytes: Math.round((duration / 1000) * 2_600_000 * (0.8 + r() * 0.4)),
      date_ms: t,
      starred: r() < 0.12,
      tags: r() < 0.15 ? ['ranked'] : r() < 0.08 ? ['funny', 'squad'] : [],
      info: {
        duration_ms: duration,
        created_unix: Math.floor(t / 1000),
        codec: g === 4 ? 'HEVC' : 'H.264',
        width: ultrawide ? 2560 : 1920,
        height: 1080,
        fps: g === 1 ? 144 : 60,
        audio_tracks: 3,
        bookmarks_ms: r() < 0.2 ? [Math.round(duration * 0.4)] : [],
      },
      error: null,
      // A third have no thumbnails yet: "made" when the UI asks for them.
      thumbs: i % 3 === 2 ? { poster: null, strip: null } : { poster: 1, strip: 1 },
    });
    t -= Math.round(r() < 0.25 ? 26 * 3_600_000 * r() : 50 * 60_000 * (0.2 + r()));
  }
  clips.push({
    key: 'old-download.mp4',
    path: 'C:\\Users\\you\\Videos\\Klip\\old-download.mp4',
    name: 'old-download',
    game: null,
    bytes: 4_200_000,
    date_ms: now - 40 * 86_400_000,
    starred: false,
    tags: [],
    info: null,
    error: 'malformed file: no moov box',
    thumbs: { poster: null, strip: null },
  });
  return clips;
}

function hueOf(key: string): number {
  const game = key.split('/')[0] ?? '';
  const g = GAMES.indexOf(game);
  let h = 0;
  for (const ch of key) h = (h * 31 + ch.charCodeAt(0)) % 360;
  return g >= 0 ? (HUES[g] ?? 0) + (h % 40) - 20 : h;
}

function svgUrl(svg: string): string {
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

/** A frame: sky, ground and a marker that moves with `phase` (0…1). */
function frame(hue: number, phase: number, y: number, label: string): string {
  const x = 30 + phase * 260;
  return `<g transform="translate(0 ${y})">
    <rect width="320" height="180" fill="hsl(${hue} 45% 22%)"/>
    <rect y="110" width="320" height="70" fill="hsl(${hue} 35% 12%)"/>
    <circle cx="${x}" cy="${70 + Math.sin(phase * 6.28) * 25}" r="16" fill="hsl(${hue} 80% 62%)"/>
    <text x="12" y="166" font-family="Consolas, monospace" font-size="14" fill="rgba(255,255,255,0.75)">${label}</text>
  </g>`;
}

function thumb(kind: ThumbKind, key: string): string {
  const hue = hueOf(key);
  if (kind === 'poster') {
    return svgUrl(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 180">${frame(hue, 0.97, 0, '')}</svg>`);
  }
  let body = '';
  for (let i = 0; i < TILES; i += 1) body += frame(hue, (i + 0.5) / TILES, i * 180, `frame ${i + 1}/${TILES}`);
  return svgUrl(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 ${180 * TILES}">${body}</svg>`);
}

interface Args {
  search?: string;
  game?: string;
  starred?: boolean;
  tag?: string;
  since_ms?: number;
  sort?: string;
  keys?: string[];
  path?: string;
  paths?: string[];
  name?: string;
  tags?: string[];
}

export function mockLibrary(emit: (e: KlipEvent) => void) {
  const clips = makeClips();
  const cache = new Map<string, string>();
  setThumbProvider((kind, key, v) => {
    const id = `${kind}:${key}:${v}`;
    let url = cache.get(id);
    if (!url) {
      url = thumb(kind, key);
      cache.set(id, url);
    }
    return url;
  });
  const byPath = (p: string | undefined) => {
    const c = clips.find((x) => x.path === p);
    if (!c) throw new CommandError('bad_args', 'no such clip');
    return c;
  };

  function list(a: Args): Listing {
    const words = (a.search ?? '').toLowerCase().split(/\s+/).filter(Boolean);
    let out = clips.filter((c) => {
      if (a.starred && !c.starred) return false;
      if (a.game !== undefined && (c.game ?? '') !== a.game) return false;
      if (a.tag && !c.tags.some((t) => t.toLowerCase() === a.tag?.toLowerCase())) return false;
      if (a.since_ms && c.date_ms < a.since_ms) return false;
      const hay = [c.name, c.game ?? '', ...c.tags].join('\n').toLowerCase();
      return words.every((w) => hay.includes(w));
    });
    const dur = (c: LibClip) => c.info?.duration_ms ?? 0;
    const cmp: Record<string, (x: LibClip, y: LibClip) => number> = {
      newest: (x, y) => y.date_ms - x.date_ms,
      oldest: (x, y) => x.date_ms - y.date_ms,
      longest: (x, y) => dur(y) - dur(x),
      shortest: (x, y) => dur(x) - dur(y),
      largest: (x, y) => y.bytes - x.bytes,
      name: (x, y) => x.name.toLowerCase().localeCompare(y.name.toLowerCase()),
    };
    out = [...out].sort(cmp[a.sort ?? 'newest'] ?? cmp.newest);
    const games = new Map<string, number>();
    const tags = new Map<string, number>();
    for (const c of clips) {
      games.set(c.game ?? '', (games.get(c.game ?? '') ?? 0) + 1);
      for (const t of c.tags) tags.set(t, (tags.get(t) ?? 0) + 1);
    }
    return {
      folder: 'C:\\Users\\you\\Videos\\Klip',
      clips: structuredClone(out),
      matched: out.length,
      total: clips.length,
      total_bytes: clips.reduce((s, c) => s + c.bytes, 0),
      starred: clips.filter((c) => c.starred).length,
      games: [...games].sort((x, y) => x[0].localeCompare(y[0])).map(([name, count]) => ({ name, count })),
      tags: [...tags].sort((x, y) => y[1] - x[1]).map(([name, count]) => ({ name, count })),
      strip: { tiles: TILES, tile_width: 240 },
      thumbs_pending: clips.filter((c) => !c.thumbs.poster && !c.error).length,
    };
  }

  return function call(cmd: string, args: unknown): unknown {
    const a = (args ?? {}) as Args;
    switch (cmd) {
      case 'library.list':
        return list(a);
      case 'library.thumbs': {
        const keys = a.keys ?? [];
        keys.forEach((k, i) =>
          setTimeout(
            () => {
              const c = clips.find((x) => x.key === k);
              if (!c || c.error) return;
              c.thumbs = { poster: 2, strip: 2 };
              emit({
                event: 'thumbs_ready',
                key: k,
                poster: 2,
                strip: 2,
                failed: false,
              });
            },
            300 + i * 120,
          ),
        );
        return { queued: keys.length };
      }
      case 'clip.star': {
        const c = byPath(a.path);
        c.starred = !!(args as { starred?: boolean }).starred;
        emit({ event: 'library_changed', reason: 'starred' });
        return { starred: c.starred };
      }
      case 'clip.tags': {
        const c = byPath(a.path);
        const seen = new Set<string>();
        c.tags = (a.tags ?? [])
          .map((t) => t.trim().replace(/\s+/g, ' '))
          .filter((t) => t && !seen.has(t.toLowerCase()) && seen.add(t.toLowerCase()));
        if (c.tags.some((t) => t.length > 40)) throw new CommandError('bad_args', 'tag is longer than 40 characters');
        emit({ event: 'library_changed', reason: 'tags' });
        return { tags: c.tags };
      }
      case 'clip.rename': {
        const c = byPath(a.path);
        const name = (a.name ?? '').trim().replace(/\.mp4$/i, '');
        if (!name) throw new CommandError('bad_args', 'the name is empty');
        const bad = /[<>:"/\\|?*]/.exec(name);
        if (bad) throw new CommandError('bad_args', `a name can't contain ${bad[0]}`);
        const dir = c.key.includes('/') ? `${c.key.split('/')[0]}/` : '';
        const key = `${dir}${name}.mp4`;
        if (clips.some((x) => x !== c && x.key.toLowerCase() === key.toLowerCase())) {
          throw new CommandError('bad_args', `there is already a clip called ${name}.mp4`);
        }
        c.key = key;
        c.name = name;
        c.path = c.path.replace(/[^\\]+$/, `${name}.mp4`);
        emit({ event: 'library_changed', reason: 'renamed' });
        return { path: c.path, key, name };
      }
      case 'clip.delete': {
        const paths = new Set(a.paths ?? []);
        const before = clips.length;
        for (let i = clips.length - 1; i >= 0; i -= 1) if (paths.has(clips[i]?.path ?? '')) clips.splice(i, 1);
        emit({ event: 'library_changed', reason: 'deleted' });
        return {
          removed: before - clips.length,
          bytes: 0,
          to_trash: true,
          failed: [],
        };
      }
      case 'clip.open':
      case 'clip.reveal':
        byPath(a.path);
        return { opened: true };
      default:
        return undefined;
    }
  };
}
