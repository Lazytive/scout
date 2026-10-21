// The update check and updater (ADR 0022): pure helpers for what the UI says.

import type { KlipEvent, UpdateStatus } from './types';

export type UpdateStatusEvent = Extract<KlipEvent, { event: 'update_status' }>;

/** The `update_status` event as a status (its `event` key dropped). */
export function statusOf(e: UpdateStatusEvent): UpdateStatus {
  const { state, current, version, notes, date, url, progress, error, checked_ms, installs } = e;
  return { state, current, version, notes, date, url, progress, error, checked_ms, installs };
}

/** The update prompt's title. */
export function updateTitle(u: UpdateStatus): string {
  const v = u.version ?? '';
  switch (u.state) {
    case 'downloading':
      return `Downloading Scout ${v}…`;
    case 'installing':
      return `Installing Scout ${v}: Scout restarts in a moment`;
    default:
      return `Scout ${v} is available`;
  }
}

/** `ms` ago, roughly: "just now", "5 min ago", "3 h ago", "2 days ago". */
export function ago(ms: number): string {
  const min = Math.max(0, Math.round(ms / 60_000));
  if (min < 1) return 'just now';
  if (min < 60) return `${min} min ago`;
  const h = Math.round(min / 60);
  if (h < 48) return `${h} h ago`;
  return `${Math.round(h / 24)} days ago`;
}

/** Settings → General: one line on where things stand, at `now` (ms since the epoch). */
export function updateLine(u: UpdateStatus | null, now: number): string {
  if (!u || u.state === 'idle') return `Scout ${u?.current ?? ''} · not checked yet`.replace('  ', ' ');
  const when = u.checked_ms ? ` · checked ${ago(now - u.checked_ms)}` : '';
  switch (u.state) {
    case 'checking':
      return `Scout ${u.current} · checking…`;
    case 'up_to_date':
      return `Scout ${u.current} is up to date${when}`;
    case 'available':
      return `Scout ${u.version} is available (this is ${u.current})${when}`;
    case 'downloading':
      return `Downloading Scout ${u.version}: ${u.progress ?? 0} %`;
    case 'installing':
      return `Installing Scout ${u.version}…`;
    case 'failed':
      return `Scout ${u.current} · the last check failed: ${u.error ?? 'unknown error'}${when}`;
    default:
      return `Scout ${u.current}`;
  }
}

/** The title bar shows the Update button in these states. */
export function updatePending(u: UpdateStatus | null): boolean {
  return !!u && (u.state === 'available' || u.state === 'downloading' || u.state === 'installing');
}
