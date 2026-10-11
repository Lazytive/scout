// Home's recent clips (M11.1): the newest few clips of the library, apart from the
// Library page's query and selection. Listed while Home is on screen, again on the
// engine's library events.

import { call } from './api';
import { RECENT_COUNT, recentArgs, withThumbs, type LibClip, type Listing } from './library';
import type { KlipEvent } from './types';

export const recent = $state({
  clips: [] as LibClip[],
  /** Frames in a hover-scrub strip (the engine's number). */
  tiles: 16,
  /** A listing came back at least once (before it: no "no clips yet" message). */
  loaded: false,
  error: null as string | null,
  /** Home is on screen (only then do events list again). */
  active: false,
});

let seq = 0;

export async function refreshRecent(): Promise<void> {
  const mine = ++seq;
  try {
    const l = await call<Listing>('library.list', recentArgs());
    if (mine !== seq) return;
    recent.clips = l.clips.slice(0, RECENT_COUNT);
    recent.tiles = l.strip.tiles;
    recent.loaded = true;
    recent.error = null;
  } catch (e) {
    if (mine === seq) recent.error = e instanceof Error ? e.message : String(e);
  }
}

let timer: ReturnType<typeof setTimeout> | null = null;

function refreshSoon(ms: number): void {
  if (timer) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = null;
    void refreshRecent();
  }, ms);
}

/** Engine events that change the recent clips. */
export function recentEvent(e: KlipEvent): void {
  if (!recent.active) return;
  switch (e.event) {
    case 'thumbs_ready':
      recent.clips = withThumbs(recent.clips, e.key, e.poster, e.strip, e.failed);
      break;
    case 'clip_saved':
    case 'storage_cleaned':
    case 'library_changed':
    case 'recording_stopped':
    case 'settings_changed':
      refreshSoon(300);
      break;
    default:
      break;
  }
}
