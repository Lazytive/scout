import { beforeEach, describe, expect, it, vi } from 'vitest';

// The store's actions against a fake engine (`./api`) and a toast recorder.
const fake = vi.hoisted(() => ({
  calls: [] as { cmd: string; args: unknown }[],
  toasts: [] as string[],
  failPath: '',
}));

vi.mock('./api', () => ({
  call: (cmd: string, args: unknown) => {
    fake.calls.push({ cmd, args });
    if (cmd === 'clip.star' && (args as { path: string }).path === fake.failPath) {
      return Promise.reject(new Error('the file is locked'));
    }
    if (cmd === 'library.list') return Promise.resolve({ clips: [], total: 0 });
    return Promise.resolve({});
  },
  CommandError: class CommandError extends Error {},
  dragFilesOut: () => Promise.resolve(),
}));
vi.mock('./toasts.svelte', () => ({
  toast: (text: string, kind = 'ok') => fake.toasts.push(`${kind}: ${text}`),
}));

import type { LibClip } from './library';
import { setStar } from './library.svelte';

const clip = (path: string) => ({ key: path, path, name: path, starred: false }) as unknown as LibClip;

beforeEach(() => {
  fake.calls = [];
  fake.toasts = [];
  fake.failPath = '';
});

describe('setStar', () => {
  it('stars the rest of the selection when one clip fails (IMPROVEMENTS B7)', async () => {
    fake.failPath = 'b.mp4';
    await setStar([clip('a.mp4'), clip('b.mp4'), clip('c.mp4')], true);
    const starred = fake.calls.filter((c) => c.cmd === 'clip.star').map((c) => (c.args as { path: string }).path);
    expect(starred).toEqual(['a.mp4', 'b.mp4', 'c.mp4']);
    expect(fake.toasts).toEqual(["error: Couldn't star 1 of 3 clips: the file is locked"]);
    expect(fake.calls.at(-1)?.cmd).toBe('library.list');
  });

  it('says nothing when every clip worked', async () => {
    await setStar([clip('a.mp4')], false);
    expect(fake.toasts).toEqual([]);
  });

  it('names a single clip', async () => {
    fake.failPath = 'a.mp4';
    await setStar([clip('a.mp4')], false);
    expect(fake.toasts).toEqual(["error: Couldn't unstar the clip: the file is locked"]);
  });
});
