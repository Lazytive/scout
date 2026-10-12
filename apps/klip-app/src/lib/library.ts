// Library helpers (pure, unit-tested in library.test.ts): names, day groups, hover-scrub
// math, selection, date filters. The engine does the searching and sorting
// (`library.list`); these only shape what it returns for the grid.

export interface ClipInfo {
  duration_ms: number | null;
  created_unix: number;
  codec: string | null;
  width: number;
  height: number;
  fps: number | null;
  audio_tracks: number;
  bookmarks_ms: number[];
}

export interface ThumbVersions {
  poster: number | null;
  strip: number | null;
}

export interface LibClip {
  key: string;
  path: string;
  name: string;
  game: string | null;
  bytes: number;
  date_ms: number;
  starred: boolean;
  tags: string[];
  info: ClipInfo | null;
  error: string | null;
  thumbs: ThumbVersions;
}

export interface Facet {
  name: string;
  count: number;
}

export interface Listing {
  folder: string;
  clips: LibClip[];
  matched: number;
  total: number;
  total_bytes: number;
  starred: number;
  games: Facet[];
  tags: Facet[];
  strip: { tiles: number; tile_width: number };
  thumbs_pending: number;
}

export type Sort = 'newest' | 'oldest' | 'longest' | 'shortest' | 'largest' | 'name';
export type DateRange = 'all' | 'today' | 'week' | 'month';

export interface Query {
  search: string;
  game: string | null;
  starred: boolean;
  tag: string | null;
  range: DateRange;
  sort: Sort;
}

export const defaultQuery: Query = {
  search: '',
  game: null,
  starred: false,
  tag: null,
  range: 'all',
  sort: 'newest',
};

/** Start of the date range in ms since the epoch (local days), or `null` for all. */
export function sinceFor(range: DateRange, now: Date): number | null {
  const midnight = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  const day = 86_400_000;
  switch (range) {
    case 'today':
      return midnight;
    case 'week':
      return midnight - 6 * day;
    case 'month':
      return midnight - 29 * day;
    default:
      return null;
  }
}

/** The `library.list` arguments for a query (`nowMs`: the current time). */
export function listArgs(q: Query, nowMs: number): Record<string, unknown> {
  const args: Record<string, unknown> = { sort: q.sort };
  if (q.search.trim()) args.search = q.search.trim();
  if (q.game !== null) args.game = q.game;
  if (q.starred) args.starred = true;
  if (q.tag !== null) args.tag = q.tag;
  const since = sinceFor(q.range, new Date(nowMs));
  if (since !== null) args.since_ms = since;
  return args;
}

/** Whether any filter (not the sort) is set. */
export function filtered(q: Query): boolean {
  return q.search.trim() !== '' || q.game !== null || q.starred || q.tag !== null || q.range !== 'all';
}

// Klip_… are the names from before the rename to Scout (ADR 0020).
const DEFAULT_NAME = /^(?:Scout|Klip)(_Recording)?_(\d{4})-(\d{2})-(\d{2})_(\d{2})-(\d{2})-(\d{2})(?:_\d+)?$/;

/** What a card calls a clip: Scout's own file names become `Clip 18:22:01` /
    `Recording 18:22:01` (clips are often saved seconds apart); names people gave stay as
    they are. */
/** What `share.discord` answers (M11.5): copied as is, or an export making a copy that
 *  fits (it goes on the clipboard when done). */
export type DiscordResult =
  { action: 'copied' } | { action: 'export'; id: number; out: string; reason: string; running?: boolean };

/** The toast once a clip is on the clipboard for Discord. */
export function discordReadyText(asIs: boolean): string {
  return asIs
    ? 'Copied: it already fits Discord. Paste it there'
    : 'Ready: the Discord copy is on the clipboard. Paste it in Discord';
}

/** The toast after copying `n` clips as files (M11.4). */
export function copiedText(n: number): string {
  return n === 1
    ? 'Copied: paste it in Discord, a chat or a folder'
    : `Copied ${n} clips: paste them in Discord, a chat or a folder`;
}

/** Which clips a drag that starts on `key` takes: the whole selection when `key` is in
 *  it (in selection order), else only that clip. */
export function dragGroup(key: string, selected: string[]): string[] {
  return selected.includes(key) ? selected : [key];
}

export function displayName(name: string): string {
  const m = DEFAULT_NAME.exec(name);
  if (!m) return name;
  return `${m[1] ? 'Recording' : 'Clip'} ${m[5]}:${m[6]}:${m[7]}`;
}

function startOfDay(d: Date): number {
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** The day heading for a date: Today, Yesterday, a weekday this week, else the date. */
export function dayLabel(ms: number, now: Date, locale?: string): string {
  const d = new Date(ms);
  const days = Math.round((startOfDay(now) - startOfDay(d)) / 86_400_000);
  if (days === 0) return 'Today';
  if (days === 1) return 'Yesterday';
  if (days > 1 && days < 7) return d.toLocaleDateString(locale, { weekday: 'long' });
  const sameYear = d.getFullYear() === now.getFullYear();
  return d.toLocaleDateString(locale, {
    day: 'numeric',
    month: 'long',
    ...(sameYear ? {} : { year: 'numeric' }),
  });
}

export interface Group {
  /** `null`: no heading (sorts other than by date). */
  label: string | null;
  key: string;
  clips: LibClip[];
}

/** Day groups for date sorts (clips keep their order); one unnamed group otherwise. */
export function groupClips(clips: LibClip[], sort: Sort, now: Date, locale?: string): Group[] {
  if (sort !== 'newest' && sort !== 'oldest') return [{ label: null, key: 'all', clips }];
  const groups: Group[] = [];
  for (const c of clips) {
    const key = String(startOfDay(new Date(c.date_ms)));
    const last = groups[groups.length - 1];
    if (last && last.key === key) last.clips.push(c);
    else groups.push({ label: dayLabel(c.date_ms, now, locale), key, clips: [c] });
  }
  return groups;
}

/** Which strip frame a pointer at `x` of `width` shows (0 … tiles − 1). */
export function scrubTile(x: number, width: number, tiles: number): number {
  if (!(width > 0) || tiles <= 1) return 0;
  const t = Math.floor((x / width) * tiles);
  return Math.min(tiles - 1, Math.max(0, t));
}

/** Where in the clip frame `tile` was taken (the middle of its slice), in ms. */
export function tileTime(tile: number, tiles: number, durationMs: number): number {
  if (tiles <= 0 || !(durationMs > 0)) return 0;
  return Math.round((durationMs * (2 * tile + 1)) / (2 * tiles));
}

/** CSS `background-position-y` for a frame of a vertical strip. */
export function stripPosition(tile: number, tiles: number): string {
  if (tiles <= 1) return '0%';
  return `${(tile / (tiles - 1)) * 100}%`;
}

/** The size of a picture with `aspect` fitted into a 16:9 box, in % of the box. */
export function fitBox(width: number, height: number): { w: number; h: number } {
  if (!(width > 0) || !(height > 0)) return { w: 100, h: 100 };
  const box = 16 / 9;
  const aspect = width / height;
  if (Math.abs(aspect - box) < 0.01) return { w: 100, h: 100 };
  return aspect > box ? { w: 100, h: (box / aspect) * 100 } : { w: (aspect / box) * 100, h: 100 };
}

/** `1080p60`, `1440p144`, `2560×1080`; empty when unknown. */
export function qualityLabel(info: ClipInfo | null): string {
  if (!info || !info.width || !info.height) return '';
  const fps = info.fps ? Math.round(info.fps) : null;
  const standard = Math.abs(info.width / info.height - 16 / 9) < 0.02;
  const res = standard ? `${info.height}p` : `${info.width}×${info.height}`;
  return fps ? `${res}${standard ? '' : ' '}${fps}${standard ? '' : ' fps'}` : res;
}

/** A new selection after a click on `key` (`order`: keys as shown). `anchor` is the
    clip a Shift range starts from. */
export function clickSelect(
  selected: string[],
  anchor: string | null,
  key: string,
  order: string[],
  mods: { ctrl: boolean; shift: boolean },
): { selected: string[]; anchor: string } {
  if (mods.shift && anchor !== null) {
    const a = order.indexOf(anchor);
    const b = order.indexOf(key);
    if (a >= 0 && b >= 0) {
      const [lo, hi] = a < b ? [a, b] : [b, a];
      const range = order.slice(lo, hi + 1);
      const base = mods.ctrl ? selected.filter((k) => !range.includes(k)) : [];
      return { selected: [...base, ...range], anchor };
    }
  }
  if (mods.ctrl) {
    return selected.includes(key)
      ? { selected: selected.filter((k) => k !== key), anchor: key }
      : { selected: [...selected, key], anchor: key };
  }
  return { selected: [key], anchor: key };
}

/** The key an arrow key moves to in a grid of `columns` columns (`null`: stay). */
export function moveKey(
  current: string | null,
  order: string[],
  columns: number,
  dir: 'left' | 'right' | 'up' | 'down' | 'home' | 'end',
): string | null {
  if (order.length === 0) return null;
  const i = current === null ? -1 : order.indexOf(current);
  if (i < 0) return order[0] ?? null;
  const cols = Math.max(1, columns);
  const step = {
    left: -1,
    right: 1,
    up: -cols,
    down: cols,
    home: -i,
    end: order.length - 1 - i,
  }[dir];
  const j = Math.min(order.length - 1, Math.max(0, i + step));
  return order[j] ?? null;
}

/** Keep only selected keys that are still listed. */
export function pruneSelection(selected: string[], clips: LibClip[]): string[] {
  const keys = new Set(clips.map((c) => c.key));
  return selected.filter((k) => keys.has(k));
}
