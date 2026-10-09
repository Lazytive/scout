import { describe, expect, it } from 'vitest';

import { actionLabel, describeEvent, formatBytes, formatDuration, formatTimecode } from './format';
import type { Game } from './types';

describe('formatBytes', () => {
  it('uses binary units', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(1023)).toBe('1023 B');
    expect(formatBytes(1536)).toBe('1.5 KB');
    expect(formatBytes(340 * 1024 * 1024)).toBe('340 MB');
    expect(formatBytes(50 * 1024 ** 3)).toBe('50.0 GB');
  });
  it('handles junk', () => {
    expect(formatBytes(-1)).toBe('–');
    expect(formatBytes(Number.NaN)).toBe('–');
  });
});

describe('formatDuration', () => {
  it('reads naturally', () => {
    expect(formatDuration(45_000)).toBe('45 s');
    expect(formatDuration(120_000)).toBe('2 min');
    expect(formatDuration(150_000)).toBe('2 min 30 s');
    expect(formatDuration(3_900_000)).toBe('1 h 05 min');
    expect(formatDuration(-5)).toBe('–');
  });
});

describe('formatTimecode', () => {
  it('pads hours, minutes and seconds', () => {
    expect(formatTimecode(0)).toBe('00:00:00');
    expect(formatTimecode(105_400)).toBe('00:01:45');
    expect(formatTimecode(3_723_000)).toBe('01:02:03');
    expect(formatTimecode(Number.NaN)).toBe('--:--:--');
  });
});

describe('actionLabel', () => {
  it('names hotkey actions', () => {
    expect(actionLabel('clip.save', 30)).toBe('Save the last 30 s');
    expect(actionLabel('clip.save', 120)).toBe('Save the last 2 min');
    expect(actionLabel('session.toggle', 0)).toBe('Start / stop recording');
    expect(actionLabel('nope', null)).toBe('Unknown action');
  });
});

describe('describeEvent', () => {
  const game: Game = {
    pid: 1,
    exe: 'eldenring.exe',
    key: 'eldenring.exe',
    name: 'ELDEN RING',
    window: 5,
    source: { kind: 'library', store: 'steam' },
    foreground: true,
  };
  it('describes what users care about', () => {
    expect(describeEvent({ event: 'game_started', game })?.text).toBe('ELDEN RING started');
    const saved = describeEvent({
      event: 'clip_saved',
      game: 'ELDEN RING',
      window: 5,
      clip: { path: 'x.mp4', duration_ms: 30_000, bytes: 1, frames: 1, save_ms: 5, bookmarks: 0 },
    });
    expect(saved).toEqual({ text: 'Clip saved: 30 s of ELDEN RING', tone: 'good' });
    expect(
      describeEvent({ event: 'action_failed', action: 'clip.save', code: 'not_ready', message: 'no game is running' })
        ?.tone,
    ).toBe('warn');
  });
  it('skips internal events', () => {
    expect(describeEvent({ event: 'hotkeys', bound: [] })).toBeNull();
    expect(describeEvent({ event: 'settings_changed', settings: {} })).toBeNull();
  });
});
