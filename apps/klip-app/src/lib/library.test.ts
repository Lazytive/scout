import { describe, expect, it } from 'vitest';

import {
  clickSelect,
  dayLabel,
  defaultQuery,
  displayName,
  filtered,
  fitBox,
  groupClips,
  listArgs,
  moveKey,
  pruneSelection,
  qualityLabel,
  scrubTile,
  sinceFor,
  stripPosition,
  tileTime,
  type LibClip,
} from './library';

const now = new Date(2026, 9, 9, 15, 30); // Friday 9 October 2026, 15:30 local

function clip(key: string, date: Date): LibClip {
  return {
    key,
    path: `C:\\clips\\${key}`,
    name: key.replace(/\.mp4$/, ''),
    game: null,
    bytes: 1,
    date_ms: date.getTime(),
    starred: false,
    tags: [],
    info: null,
    error: null,
    thumbs: { poster: null, strip: null },
  };
}

describe('names', () => {
  it('turns Scout file names (and Klip ones from before the rename) into short titles and keeps others', () => {
    expect(displayName('Scout_2026-10-11_18-22-01')).toBe('Clip 18:22:01');
    expect(displayName('Scout_Recording_2026-10-11_07-05-59_3')).toBe('Recording 07:05:59');
    expect(displayName('Klip_2026-10-09_18-22-01')).toBe('Clip 18:22:01');
    expect(displayName('Klip_2026-10-09_18-22-01_2')).toBe('Clip 18:22:01');
    expect(displayName('Klip_Recording_2026-10-09_07-05-59')).toBe('Recording 07:05:59');
    expect(displayName('Ace clutch')).toBe('Ace clutch');
    expect(displayName('Klip_2026-10-09')).toBe('Klip_2026-10-09');
  });

  it('labels resolution and frame rate', () => {
    const info = {
      duration_ms: 1,
      created_unix: 0,
      codec: 'H.264',
      width: 1920,
      height: 1080,
      fps: 59.94,
      audio_tracks: 1,
      bookmarks_ms: [],
    };
    expect(qualityLabel(info)).toBe('1080p60');
    expect(qualityLabel({ ...info, width: 2560, height: 1080, fps: 144 })).toBe('2560×1080 144 fps');
    expect(qualityLabel({ ...info, fps: null })).toBe('1080p');
    expect(qualityLabel(null)).toBe('');
  });
});

describe('days', () => {
  it('names days relative to today', () => {
    expect(dayLabel(new Date(2026, 9, 9, 1).getTime(), now, 'en-GB')).toBe('Today');
    expect(dayLabel(new Date(2026, 9, 8, 23).getTime(), now, 'en-GB')).toBe('Yesterday');
    expect(dayLabel(new Date(2026, 9, 5, 12).getTime(), now, 'en-GB')).toBe('Monday');
    expect(dayLabel(new Date(2026, 8, 30, 12).getTime(), now, 'en-GB')).toBe('30 September');
    expect(dayLabel(new Date(2025, 11, 31, 12).getTime(), now, 'en-GB')).toBe('31 December 2025');
  });

  it('groups by day for date sorts only', () => {
    const clips = [
      clip('a.mp4', new Date(2026, 9, 9, 14)),
      clip('b.mp4', new Date(2026, 9, 9, 9)),
      clip('c.mp4', new Date(2026, 9, 8, 22)),
    ];
    const g = groupClips(clips, 'newest', now, 'en-GB');
    expect(g.map((x) => [x.label, x.clips.length])).toEqual([
      ['Today', 2],
      ['Yesterday', 1],
    ]);
    expect(groupClips(clips, 'longest', now)).toEqual([{ label: null, key: 'all', clips }]);
  });

  it('turns date ranges into local-midnight starts', () => {
    expect(sinceFor('all', now)).toBeNull();
    expect(sinceFor('today', now)).toBe(new Date(2026, 9, 9).getTime());
    expect(sinceFor('week', now)).toBe(new Date(2026, 9, 3).getTime());
    expect(sinceFor('month', now)).toBe(new Date(2026, 8, 10).getTime());
  });

  it('builds list arguments with only what is set', () => {
    expect(listArgs(defaultQuery, now.getTime())).toEqual({ sort: 'newest' });
    expect(filtered(defaultQuery)).toBe(false);
    const q = {
      ...defaultQuery,
      search: '  ace ',
      game: '',
      starred: true,
      tag: 'ranked',
      range: 'today' as const,
    };
    expect(listArgs(q, now.getTime())).toEqual({
      sort: 'newest',
      search: 'ace',
      game: '',
      starred: true,
      tag: 'ranked',
      since_ms: new Date(2026, 9, 9).getTime(),
    });
    expect(filtered(q)).toBe(true);
  });
});

describe('hover-scrub', () => {
  it('maps the pointer to a strip frame', () => {
    expect(scrubTile(0, 320, 16)).toBe(0);
    expect(scrubTile(19.9, 320, 16)).toBe(0);
    expect(scrubTile(20, 320, 16)).toBe(1);
    expect(scrubTile(319, 320, 16)).toBe(15);
    expect(scrubTile(400, 320, 16)).toBe(15);
    expect(scrubTile(-5, 320, 16)).toBe(0);
    expect(scrubTile(10, 0, 16)).toBe(0);
  });

  it('knows when each frame was taken and where it is in the strip', () => {
    expect(tileTime(0, 16, 32_000)).toBe(1000);
    expect(tileTime(15, 16, 32_000)).toBe(31_000);
    expect(tileTime(3, 0, 32_000)).toBe(0);
    expect(stripPosition(0, 16)).toBe('0%');
    expect(stripPosition(15, 16)).toBe('100%');
    expect(stripPosition(5, 16)).toBe(`${(5 / 15) * 100}%`);
  });

  it('fits other aspect ratios into the 16:9 card', () => {
    expect(fitBox(1920, 1080)).toEqual({ w: 100, h: 100 });
    const wide = fitBox(2560, 1080);
    expect(wide.w).toBe(100);
    expect(wide.h).toBeCloseTo(75, 0);
    const tall = fitBox(1080, 1920);
    expect(tall.h).toBe(100);
    expect(tall.w).toBeCloseTo(31.6, 0);
    expect(fitBox(0, 0)).toEqual({ w: 100, h: 100 });
  });
});

describe('selection', () => {
  const order = ['a', 'b', 'c', 'd', 'e'];

  it('selects, toggles and selects ranges', () => {
    let s = clickSelect([], null, 'b', order, { ctrl: false, shift: false });
    expect(s).toEqual({ selected: ['b'], anchor: 'b' });
    s = clickSelect(s.selected, s.anchor, 'd', order, {
      ctrl: false,
      shift: true,
    });
    expect(s.selected).toEqual(['b', 'c', 'd']);
    s = clickSelect(s.selected, s.anchor, 'c', order, {
      ctrl: true,
      shift: false,
    });
    expect(s.selected).toEqual(['b', 'd']);
    s = clickSelect(s.selected, s.anchor, 'a', order, {
      ctrl: true,
      shift: true,
    });
    expect(s.selected.sort()).toEqual(['a', 'b', 'c', 'd']);
    expect(clickSelect(['a', 'b'], 'a', 'e', order, { ctrl: false, shift: false }).selected).toEqual(['e']);
  });

  it('moves with arrow keys in a grid', () => {
    expect(moveKey(null, order, 2, 'right')).toBe('a');
    expect(moveKey('a', order, 2, 'right')).toBe('b');
    expect(moveKey('a', order, 2, 'down')).toBe('c');
    expect(moveKey('e', order, 2, 'down')).toBe('e');
    expect(moveKey('d', order, 2, 'up')).toBe('b');
    expect(moveKey('a', order, 2, 'left')).toBe('a');
    expect(moveKey('c', order, 2, 'end')).toBe('e');
    expect(moveKey('c', order, 2, 'home')).toBe('a');
    expect(moveKey('a', [], 2, 'right')).toBeNull();
  });

  it('drops selected clips that are gone', () => {
    expect(pruneSelection(['a', 'x'], [clip('a', now)])).toEqual(['a']);
  });
});
