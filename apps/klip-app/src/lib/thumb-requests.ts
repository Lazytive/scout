// Thumbnails for the cards on screen: keys are collected for a moment, then asked for in
// one `library.thumbs` call (the engine makes them first, before the background work).

import { call } from './api';

const asked = new Set<string>();
let wanted = new Set<string>();
let timer: ReturnType<typeof setTimeout> | null = null;

function flush(): void {
  timer = null;
  const all = [...wanted];
  const keys = all.slice(0, 256);
  wanted = new Set(all.slice(256));
  keys.forEach((k) => asked.add(k));
  void call('library.thumbs', { keys }).catch(() => keys.forEach((k) => asked.delete(k)));
  if (wanted.size > 0) timer = setTimeout(flush, 120);
}

/** A card without thumbnails came into view. */
export function wantThumbs(key: string): void {
  if (asked.has(key)) return;
  wanted.add(key);
  timer ??= setTimeout(flush, 120);
}
