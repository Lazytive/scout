import { describe, expect, it } from 'vitest';

import { applyShorts, clampCustom, cropRect, gaps, keepAspect, msToPct, newText, outputSize, progressPct, pxToMs, tileFor, trimText } from './editor';
import { newProject } from './timeline';

describe('crop', () => {
  it('wide is the whole frame, tall is a centred 9:16 window with even sides', () => {
    expect(cropRect('wide', 0.5, 1920, 1080)).toEqual({ x: 0, y: 0, width: 1920, height: 1080 });
    // 1080 * 9 / 16 = 607.5 → 606; spare 1314, centred → 657 → 656.
    expect(cropRect('tall', 0.5, 1920, 1080)).toEqual({ x: 656, y: 0, width: 606, height: 1080 });
    expect(cropRect('tall', 0, 1920, 1080).x).toBe(0);
    expect(cropRect('tall', 1, 1920, 1080).x).toBe(1314);
    expect(cropRect('tall', Number.NaN, 1920, 1080).x).toBe(656);
    // Already tall: unchanged.
    expect(cropRect('tall', 0.5, 600, 1200)).toEqual({ x: 0, y: 0, width: 600, height: 1200 });
    expect(outputSize('tall', 0.5, 1920, 1080)).toEqual({ width: 606, height: 1080 });
  });
});

describe('strip geometry', () => {
  it('maps pixels, times and tiles', () => {
    expect(pxToMs(0, 400, 10_000)).toBe(0);
    expect(pxToMs(200, 400, 10_000)).toBe(5_000);
    expect(pxToMs(900, 400, 10_000)).toBe(10_000);
    expect(pxToMs(10, 0, 10_000)).toBe(0);
    expect(msToPct(2_500, 10_000)).toBe(25);
    expect(msToPct(99_000, 10_000)).toBe(100);
    expect(tileFor(0, 10_000, 16)).toBe(0);
    expect(tileFor(9_999, 10_000, 16)).toBe(15);
    expect(tileFor(5_000, 10_000, 16)).toBe(8);
    expect(tileFor(5, 0, 16)).toBe(0);
  });

  it('finds the thrown-away ranges', () => {
    const p = newProject(10_000);
    expect(gaps(p)).toEqual([]);
    p.pieces = [
      { start_ms: 1_000, end_ms: 3_000, speed: 1 },
      { start_ms: 5_000, end_ms: 9_000, speed: 2 },
    ];
    expect(gaps(p)).toEqual([
      { start_ms: 0, end_ms: 1_000 },
      { start_ms: 3_000, end_ms: 5_000 },
      { start_ms: 9_000, end_ms: 10_000 },
    ]);
  });
});

describe('text and templates', () => {
  it('a new text starts at the playhead and lasts 3 s inside the edit', () => {
    const p = newProject(4_000);
    const t = newText(p, 'pill', 2_500)!;
    expect(t).toMatchObject({ id: 1, preset: 'pill', position: 'bottom', start_ms: 2_500, end_ms: 4_000 });
    p.text.push(t);
    expect(newText(p, 'nope', 99_000)!.id).toBe(2);
    expect(newText(p, 'bold', 99_000)!.start_ms).toBe(3_999);
    expect(newText(newProject(0), 'bold', 0)).toBeNull();
  });

  it('trims control characters and caps the length', () => {
    expect(trimText('a\u0000b\r\nc')).toBe('ab\nc');
    expect(trimText('x'.repeat(500)).length).toBe(200);
  });

  it('shorts = tall, centred, 1x, the name as a caption', () => {
    const p = newProject(10_000);
    p.pieces = [{ start_ms: 0, end_ms: 10_000, speed: 2 }];
    const s = applyShorts(p, 'Klip_2026-10-10_12-00-00');
    expect(s.format).toEqual({ kind: 'tall', crop_x: 0.5 });
    expect(s.pieces[0]!.speed).toBe(1);
    expect(s.text).toHaveLength(1);
    expect(s.text[0]).toMatchObject({ preset: 'caption', position: 'bottom', start_ms: 0, end_ms: 3_000 });
    expect(p.text).toHaveLength(0);
  });

  it('progress percent is bounded', () => {
    expect(progressPct(5, 10)).toBe(50);
    expect(progressPct(5, 0)).toBe(0);
    expect(progressPct(20, 10)).toBe(100);
  });
});

describe('custom export settings', () => {
  it('clamps to what the engine takes, sides even', () => {
    expect(clampCustom({ width: 1281, height: 1, fps: 0, bitrate_kbps: 1e9, codec: 'vp9' as never })).toEqual({
      width: 1280,
      height: 2,
      fps: 1,
      bitrate_kbps: 200_000,
      codec: 'h264',
    });
    expect(clampCustom({ width: 9000, height: 721, fps: 59.94, bitrate_kbps: 99, codec: 'av1' })).toEqual({
      width: 4096,
      height: 720,
      fps: 60,
      bitrate_kbps: 100,
      codec: 'av1',
    });
  });

  it('keeps the shape', () => {
    expect(keepAspect(1280, 1920, 1080, 'width')).toBe(720);
    expect(keepAspect(1920, 606, 1080, 'height')).toBe(1076);
    expect(keepAspect(100, 0, 0, 'width')).toBe(100);
  });
});
