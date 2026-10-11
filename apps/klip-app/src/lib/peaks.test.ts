import { describe, expect, it } from 'vitest';

import { PEAKS_PER_SEC, loudness, mergePeaks, peaksOf, peaksPath } from './peaks';

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

  it('shows quiet sound on a loudness scale', () => {
    expect(loudness(1)).toBe(1);
    expect(loudness(0)).toBe(0);
    // −24 dB is half the height; −60 dB doesn't show.
    expect(loudness(10 ** (-24 / 20))).toBeCloseTo(0.5);
    expect(loudness(0.001)).toBe(0);
  });

  it('draws a clip’s tracks together, the louder at each moment', () => {
    const game = new Float32Array([0.5, 0, 0.2]);
    const mic = new Float32Array([0.1, 0.8]);
    expect([...mergePeaks([game, mic])!].map((v) => Math.round(v * 10) / 10)).toEqual([0.5, 0.8, 0.2]);
    expect(mergePeaks([])).toBeNull();
  });

  it('is silent past the end of the sound', () => {
    const peaks = new Float32Array([1, 1]);
    expect(peaksPath(peaks, 5000, 6000, 1)).toBe('M0 0.5L0 0.5L1 0.5L1 0.5L0 0.5Z');
  });
});
