import { describe, expect, it } from 'vitest';

import type { UpdateStatus } from './types';
import { ago, statusOf, updateLine, updatePending, updateTitle } from './update';

const base: UpdateStatus = {
  state: 'idle',
  current: '0.1.0',
  version: null,
  notes: null,
  date: null,
  url: 'https://github.com/Lazytive/scout/releases/latest',
  progress: null,
  error: null,
  checked_ms: null,
  installs: true,
};
const NOW = 1_800_000_000_000;

describe('statusOf', () => {
  it('drops the event key and keeps every field', () => {
    const s = statusOf({ event: 'update_status', ...base, state: 'available', version: '0.2.0' });
    expect(s).toEqual({ ...base, state: 'available', version: '0.2.0' });
    expect('event' in s).toBe(false);
  });
});

describe('ago', () => {
  it('rounds to what a person would say', () => {
    expect(ago(20_000)).toBe('just now');
    expect(ago(5 * 60_000)).toBe('5 min ago');
    expect(ago(3 * 3_600_000)).toBe('3 h ago');
    expect(ago(47 * 3_600_000)).toBe('47 h ago');
    expect(ago(3 * 86_400_000)).toBe('3 days ago');
    expect(ago(-5000)).toBe('just now');
  });
});

describe('updateLine', () => {
  it('says where things stand in every state', () => {
    expect(updateLine(null, NOW)).toBe('Scout · not checked yet');
    expect(updateLine(base, NOW)).toBe('Scout 0.1.0 · not checked yet');
    expect(updateLine({ ...base, state: 'checking' }, NOW)).toBe('Scout 0.1.0 · checking…');
    expect(updateLine({ ...base, state: 'up_to_date', checked_ms: NOW - 120_000 }, NOW)).toBe(
      'Scout 0.1.0 is up to date · checked 2 min ago',
    );
    expect(updateLine({ ...base, state: 'available', version: '0.2.0', checked_ms: NOW }, NOW)).toBe(
      'Scout 0.2.0 is available (this is 0.1.0) · checked just now',
    );
    expect(updateLine({ ...base, state: 'downloading', version: '0.2.0', progress: 42 }, NOW)).toBe(
      'Downloading Scout 0.2.0: 42 %',
    );
    expect(updateLine({ ...base, state: 'installing', version: '0.2.0' }, NOW)).toBe('Installing Scout 0.2.0…');
    expect(updateLine({ ...base, state: 'failed', error: 'no connection to GitHub', checked_ms: NOW }, NOW)).toBe(
      'Scout 0.1.0 · the last check failed: no connection to GitHub · checked just now',
    );
  });
});

describe('updateTitle and updatePending', () => {
  it('name the version and what is happening to it', () => {
    expect(updateTitle({ ...base, state: 'available', version: '0.2.0' })).toBe('Scout 0.2.0 is available');
    expect(updateTitle({ ...base, state: 'downloading', version: '0.2.0' })).toBe('Downloading Scout 0.2.0…');
    expect(updateTitle({ ...base, state: 'installing', version: '0.2.0' })).toContain('Installing Scout 0.2.0');
    expect(updatePending(null)).toBe(false);
    expect(updatePending(base)).toBe(false);
    expect(updatePending({ ...base, state: 'up_to_date' })).toBe(false);
    for (const state of ['available', 'downloading', 'installing'] as const) {
      expect(updatePending({ ...base, state })).toBe(true);
    }
  });
});
