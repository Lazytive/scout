import { describe, expect, it } from 'vitest';

import { PEAKS_PER_SEC, peaksOf, peaksPath } from './peaks';

describe('waveform peaks', () => {
  it('keeps the loudest sample of each slice over all channels', () => {
    const rate = PEAKS_PER_SEC * 4; // 4 samples a slice
    const left = new Float32Array([0.1, -0.5, 0.2, 0, 0, 0, 0, 0.1]);
    const right = new Float32Array([0, 0, 0, 0, 0.3, -0.9, 0, 0]);
    expect([...peaksOf([left, right], rate)].map((v) => Math.round(v * 10) / 10)).toEqual([0.5, 0.9]);
  });

  it('draws the asked stretch mirrored around the middle', () => {
    // 1 s of peaks: the first half silent, the second half full scale.
    const peaks = new Float32Array(PEAKS_PER_SEC).map((_, i) => (i < PEAKS_PER_SEC / 2 ? 0 : 1));
    const d = peaksPath(peaks, 0, 1000, 2);
    expect(d).toBe('M0 0.5L0 0.5L1 0.5L1 0L2 0L2 1L1 1L1 0.5L0 0.5Z');
    // Only the loud half: both columns full.
    expect(peaksPath(peaks, 500, 1000, 2)).toBe('M0 0.5L0 0L1 0L1 0L2 0L2 1L1 1L1 1L0 1Z');
  });

  it('is silent past the end of the sound', () => {
    const peaks = new Float32Array([1, 1]);
    expect(peaksPath(peaks, 5000, 6000, 1)).toBe('M0 0.5L0 0.5L1 0.5L1 0.5L0 0.5Z');
  });
});
