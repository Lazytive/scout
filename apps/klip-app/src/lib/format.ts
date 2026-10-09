// Small pure helpers for display (unit-tested in format.test.ts).

import type { KlipEvent } from './types';

/** `1.2 GB`, `340 MB`, `12 KB` (binary units, like Explorer). */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n < 0) return '–';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  const digits = v >= 100 || i === 0 ? 0 : 1;
  return `${v.toFixed(digits)} ${units[i]}`;
}

/** `00:01:45` (hours:minutes:seconds), for the monospace readouts. */
export function formatTimecode(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '--:--:--';
  const s = Math.floor(ms / 1000);
  const p = (n: number) => String(n).padStart(2, '0');
  return `${p(Math.floor(s / 3600))}:${p(Math.floor(s / 60) % 60)}:${p(s % 60)}`;
}

/** `45 s`, `2 min`, `2 min 30 s`, `1 h 05 min`. */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '–';
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  const rest = s % 60;
  if (m < 60) return rest ? `${m} min ${rest} s` : `${m} min`;
  const h = Math.floor(m / 60);
  return `${h} h ${String(m % 60).padStart(2, '0')} min`;
}

const actionNames: Record<string, string> = {
  'clip.save': 'Save clip',
  'session.toggle': 'Start / stop recording',
  'bookmark.add': 'Bookmark',
  'overlay.menu': 'Quick menu',
};

/** What a hotkey does, for people. */
export function actionLabel(action: string | null, secs: number | null): string {
  if (action === 'clip.save' && secs) return `Save the last ${formatDuration(secs * 1000)}`;
  return (action && actionNames[action]) ?? 'Unknown action';
}

export type Tone = 'info' | 'good' | 'warn' | 'bad';

/** One line for the activity list, or `null` for events not worth showing. */
export function describeEvent(e: KlipEvent): { text: string; tone: Tone } | null {
  switch (e.event) {
    case 'game_started':
      return { text: `${e.game.name} started`, tone: 'info' };
    case 'game_stopped':
      return { text: `${e.game.name} closed`, tone: 'info' };
    case 'recorder_started':
      return {
        text: `Replay buffer on${e.game ? ` for ${e.game}` : ''} (${e.encoder ?? 'encoder'}, ${e.width ?? '?'}×${e.height ?? '?'} at ${e.fps} fps)`,
        tone: 'good',
      };
    case 'recorder_stopped':
      return { text: `Replay buffer off: ${e.reason}`, tone: 'info' };
    case 'recorder_failed':
      return { text: `Couldn't start recording: ${e.error}`, tone: 'bad' };
    case 'clip_saved':
      return {
        text: `Clip saved: ${formatDuration(e.clip.duration_ms)}${e.game ? ` of ${e.game}` : ''}`,
        tone: 'good',
      };
    case 'recording_started':
      return { text: 'Recording started', tone: 'good' };
    case 'recording_stopped':
      return { text: 'Recording saved', tone: 'good' };
    case 'bookmark':
      return { text: 'Bookmark added', tone: 'info' };
    case 'action_failed':
      return { text: `${actionLabel(e.action, null)}: ${e.message}`, tone: 'warn' };
    case 'storage_cleaned':
      return {
        text: `Freed ${formatBytes(e.report.bytes)} (${e.report.removed} old clips) to stay under the storage cap`,
        tone: 'info',
      };
    case 'recovered':
      return { text: 'Finished a recording left by an earlier crash', tone: 'info' };
    case 'warning':
      return { text: e.message, tone: 'warn' };
    default:
      return null;
  }
}
