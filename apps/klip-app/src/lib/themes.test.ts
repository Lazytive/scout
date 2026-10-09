/// <reference types="node" />
import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import { contrast, cssVar, luminance, resolve, textPairs, themes, uiPairs } from './themes';

describe('contrast maths', () => {
  it('matches WCAG reference values', () => {
    expect(luminance('#ffffff')).toBeCloseTo(1, 5);
    expect(luminance('#000000')).toBe(0);
    expect(contrast('#ffffff', '#000000')).toBeCloseTo(21, 5);
    expect(contrast('#767676', '#ffffff')).toBeCloseTo(4.54, 2);
  });
});

describe.each(Object.values(themes))('$label theme', (theme) => {
  it.each(textPairs)('%s on %s reads at AA (4.5:1)', (fg, bg) => {
    expect(contrast(theme.tokens[fg], theme.tokens[bg])).toBeGreaterThanOrEqual(4.5);
  });
  it.each(uiPairs)('%s against %s stands out (3:1)', (fg, bg) => {
    expect(contrast(theme.tokens[fg], theme.tokens[bg])).toBeGreaterThanOrEqual(3);
  });
});

describe('app.css', () => {
  it('holds the Dark theme as its first-paint default', () => {
    const css = readFileSync(new URL('../app.css', import.meta.url), 'utf8');
    const root = css.slice(css.indexOf(':root {'), css.indexOf('}', css.indexOf(':root {')));
    for (const [k, v] of Object.entries(themes.dark.tokens)) {
      expect(root, cssVar(k)).toContain(`${cssVar(k)}: ${v};`);
    }
  });
});

describe('resolve', () => {
  it('maps settings to themes', () => {
    expect(resolve('graphite', false).id).toBe('graphite');
    expect(resolve('system', true).id).toBe('light');
    expect(resolve('system', false).id).toBe('dark');
    expect(resolve('bogus', true).id).toBe('dark');
    expect(resolve(null, false).id).toBe('dark');
  });
  it('names tokens as CSS variables', () => {
    expect(cssVar('textMuted')).toBe('--text-muted');
    expect(cssVar('panel')).toBe('--panel');
  });
});
