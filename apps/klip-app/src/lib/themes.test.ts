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
  const css = readFileSync(new URL('../app.css', import.meta.url), 'utf8');
  const block = (start: number) => css.slice(start, css.indexOf('}', start));
  it('paints Dark first in dark mode', () => {
    const root = block(css.indexOf(':root {'));
    for (const [k, v] of Object.entries(themes.dark.tokens)) {
      expect(root, cssVar(k)).toContain(`${cssVar(k)}: ${v};`);
    }
  });
  it('paints Light first in light mode', () => {
    const media = css.indexOf('@media (prefers-color-scheme: light)');
    expect(media).toBeGreaterThan(0);
    const root = block(css.indexOf(':root {', media));
    for (const [k, v] of Object.entries(themes.light.tokens)) {
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
    expect(resolve(null, true).id).toBe('light');
    expect(resolve(undefined, false).id).toBe('dark');
  });
  it('names tokens as CSS variables', () => {
    expect(cssVar('textMuted')).toBe('--text-muted');
    expect(cssVar('panel')).toBe('--panel');
  });
});
