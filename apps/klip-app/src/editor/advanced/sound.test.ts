import { describe, expect, it } from 'vitest';

import { dbToGain, hasSeparateTracks, isVoice, rmsDb, trackWeight } from './sound';

describe('the preview mixer weighs tracks like klip_export::mixer', () => {
  const v = { game: 0.5, mic: 2 };

  it('volume_for', () => {
    expect(trackWeight('Game', true, v)).toBe(0.5);
    expect(trackWeight('System', true, v)).toBe(0.5);
    expect(trackWeight('Microphone', true, v)).toBe(2);
    expect(trackWeight('Voice', true, v)).toBe(2);
    expect(trackWeight('Mix', true, v)).toBeNull();
    expect(trackWeight('Mix', false, v)).toBe(0.5);
    expect(trackWeight('', false, v)).toBe(0.5);
  });

  it('voice and separate tracks', () => {
    expect(isVoice('Microphone')).toBe(true);
    expect(isVoice('Game')).toBe(false);
    expect(hasSeparateTracks(['Mix', 'Game'])).toBe(true);
    expect(hasSeparateTracks(['Mix', ''])).toBe(false);
  });

  it('levels', () => {
    const sine = new Float32Array(4800).map((_, i) => Math.sin((2 * Math.PI * 1000 * i) / 48_000));
    expect(rmsDb(sine)).toBeCloseTo(-3.01, 1);
    expect(rmsDb(new Float32Array(10))).toBe(-Infinity);
    expect(dbToGain(-12)).toBeCloseTo(0.2512, 4);
    expect(dbToGain(0)).toBe(1);
  });
});
