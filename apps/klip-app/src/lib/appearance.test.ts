/// <reference types="node" />
import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import {
  ACCENT_SWATCHES,
  DEFAULT_FONT,
  contrastProblems,
  defaultAppearance,
  dimColour,
  lookCss,
  mix,
  resolveLook,
  tokenKey,
  withAccent,
  type CustomTheme,
  type Env,
} from './appearance';
import { contrast, textPairs, themes, uiPairs, type Tokens } from './themes';

const env: Env = { prefersLight: false, prefersReducedMotion: false };
const url = (p: string, v?: number) => `asset://${p}${v ? `?v=${v}` : ''}`;

const neon: CustomTheme = {
  id: 'neon',
  name: 'Neon',
  base: 'light',
  colors: { panel: '#fdfdfd', 'text-muted': '#555555', accent: '#7c4dff', hover: '#ff0000', bogus: '#000000' },
  radius: 12,
  font: 'Inter',
  background: null,
};

describe('resolveLook', () => {
  it('gives the built-in themes as before when nothing else is set', () => {
    const l = resolveLook(defaultAppearance, [], env, url);
    expect(l.id).toBe('dark');
    expect(l.tokens).toEqual(themes.dark.tokens);
    expect(l.radius).toBe(6);
    expect(l.font).toBeNull();
    expect(l.textScale).toBe(1);
    expect(l.density).toBe('comfortable');
    expect(l.reduceMotion).toBe(false);
    expect(resolveLook({ theme: 'system' }, [], { ...env, prefersLight: true }, url).id).toBe('light');
    expect(resolveLook(null, [], env, url).id).toBe('dark');
  });

  it('layers a custom theme over its base, ignoring unknown keys and hover', () => {
    const l = resolveLook({ theme: 'custom', custom_theme: 'neon' }, [neon], env, url);
    expect(l.id).toBe('custom:neon');
    expect(l.scheme).toBe('light');
    expect(l.tokens.panel).toBe('#fdfdfd');
    expect(l.tokens.textMuted).toBe('#555555');
    expect(l.tokens.accent).toBe('#7c4dff');
    expect(l.tokens.hover).toBe(themes.light.tokens.hover);
    expect(l.tokens.chrome).toBe(themes.light.tokens.chrome);
    expect('bogus' in l.tokens).toBe(false);
    expect(l.radius).toBe(12);
    expect(l.font).toBe('Inter');
  });

  it("derives a theme's accent shades from its accent unless it sets them", () => {
    const t: CustomTheme = { ...neon, base: 'dark', colors: { accent: '#e0601f', focus: '#ffffff' } };
    const l = resolveLook({ theme: 'custom', custom_theme: 'neon' }, [t], env, url);
    const d = withAccent(themes.dark.tokens, 'dark', '#e0601f');
    expect(l.tokens.accentStrong).toBe(d.accentStrong);
    expect(l.tokens.accentText).toBe(d.accentText);
    expect(l.tokens.onAccent).toBe('#000000');
    expect(l.tokens.focus).toBe('#ffffff');
    expect(contrastProblems(l)).toEqual([]);
  });

  it('falls back to Follow system when the custom theme is gone', () => {
    const l = resolveLook({ theme: 'custom', custom_theme: 'gone' }, [neon], { ...env, prefersLight: true }, url);
    expect(l.id).toBe('light');
  });

  it("shows the editor's draft instead of the theme in use", () => {
    const draft = { ...neon, id: 'neon', colors: { panel: '#eeeeee' } };
    const l = resolveLook({ theme: 'dark' }, [neon], env, url, draft);
    expect(l.id).toBe('custom:neon');
    expect(l.tokens.panel).toBe('#eeeeee');
  });

  it('maps text size, density and motion', () => {
    const l = resolveLook({ text_size: 'larger', density: 'compact', motion: 'reduce' }, [], env, url);
    expect(l.textScale).toBe(1.3);
    expect(l.density).toBe('compact');
    expect(l.reduceMotion).toBe(true);
    expect(resolveLook({ text_size: 'small' }, [], env, url).textScale).toBe(0.9);
    expect(resolveLook({ motion: 'system' }, [], { ...env, prefersReducedMotion: true }, url).reduceMotion).toBe(true);
    expect(resolveLook({ motion: 'full' }, [], { ...env, prefersReducedMotion: true }, url).reduceMotion).toBe(false);
  });

  it('clamps a radius out of range and gives the picture an asset URL', () => {
    const t = { ...neon, radius: 99, background: { path: 'C:/t/neon.png', dim: 40, opacity: 70, version: 5 } };
    const l = resolveLook({ theme: 'custom', custom_theme: 'neon' }, [t], env, url);
    expect(l.radius).toBe(16);
    expect(l.background).toEqual({ url: 'asset://C:/t/neon.png?v=5', dim: 40, opacity: 70 });
  });
});

describe('accent', () => {
  it.each(Object.values(themes))('every swatch keeps AA on $label', (theme) => {
    for (const a of ACCENT_SWATCHES) {
      const t = withAccent(theme.tokens, theme.scheme, a);
      expect(t.accent).toBe(a);
      expect(contrast(t.onAccent, t.accent), `${a} on-accent`).toBeGreaterThanOrEqual(4.5);
      expect(contrast(t.onAccent, t.accentStrong), `${a} on-strong`).toBeGreaterThanOrEqual(4.5);
      for (const bg of ['panel', 'viewer', 'panelRaised'] as const) {
        expect(contrast(t.accentText, t[bg]), `${a} text on ${bg}`).toBeGreaterThanOrEqual(4.5);
      }
      expect(t.focus).toBe(t.accentText);
    }
  });

  it('uses black text on a light accent', () => {
    const t = withAccent(themes.dark.tokens, 'dark', '#ffd400');
    expect(t.onAccent).toBe('#000000');
  });

  it('a settings accent applies on top of any theme', () => {
    const l = resolveLook({ theme: 'graphite', accent: '#2e9e5b' }, [], env, url);
    expect(l.tokens.accent).toBe('#2e9e5b');
    expect(l.tokens.panel).toBe(themes.graphite.tokens.panel);
    expect(resolveLook({ accent: 'green' }, [], env, url).tokens.accent).toBe(themes.dark.tokens.accent);
  });
});

describe('contrast warning', () => {
  it('has nothing to say about the built-in themes', () => {
    for (const id of ['dark', 'graphite', 'light', 'oled'] as const) {
      expect(contrastProblems(resolveLook({ theme: id }, [], env, url))).toEqual([]);
    }
  });

  it('names the pairs below AA', () => {
    const grey: CustomTheme = { ...neon, base: 'dark', colors: { 'text-muted': '#3a3a3a' } };
    const p = contrastProblems(resolveLook({ theme: 'custom', custom_theme: 'neon' }, [grey], env, url));
    expect(p.map((x) => x.label)).toEqual(['Muted text on panels', 'Muted text on the viewer', 'Muted text on the status bar']);
    expect(p.every((x) => x.need === 4.5 && x.ratio < 4.5)).toBe(true);
  });

  it('checks translucent surfaces over the picture', () => {
    const pic: CustomTheme = { ...neon, base: 'dark', colors: {}, background: { path: 'p', dim: 0, opacity: 60 } };
    const look = resolveLook({ theme: 'custom', custom_theme: 'neon' }, [pic], env, url);
    // Over a black picture the dark surfaces only get darker: fine.
    expect(contrastProblems(look, { dark: '#000000', light: '#000000' })).toEqual([]);
    // A white area shines through 40 % of the panel: text on panels suffers.
    const p = contrastProblems(look, { dark: '#000000', light: '#ffffff' });
    expect(p.some((x) => x.bg === 'panel')).toBe(true);
    // Raised panels stay opaque, so they're never blamed on the picture.
    expect(p.some((x) => x.bg === 'panelRaised')).toBe(false);
  });

  it('only checks pairs the UI uses', () => {
    const all = new Set([...textPairs, ...uiPairs].map(([f, b]) => `${f}/${b}`));
    const p = contrastProblems(resolveLook({ theme: 'custom', custom_theme: 'x' }, [{ ...neon, id: 'x', colors: { text: '#fefefe' } }], env, url));
    for (const x of p) expect(all.has(`${x.fg}/${x.bg}`)).toBe(true);
  });
});

describe('CSS', () => {
  it('sets every token, the radius, font, scale and root attributes', () => {
    const css = lookCss(resolveLook({ theme: 'custom', custom_theme: 'neon', text_size: 'large', density: 'compact', motion: 'reduce' }, [neon], env, url));
    for (const k of Object.keys(themes.dark.tokens)) {
      expect(css.vars, k).toHaveProperty(`--${k.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}`);
    }
    expect(css.vars['--radius']).toBe('12px');
    expect(css.vars['--radius-sm']).toBe('8px');
    expect(css.vars['--font']).toBe(`"Inter", ${DEFAULT_FONT}`);
    expect(css.vars['--text-scale']).toBe('1.15');
    expect(css.vars['--backdrop-image']).toBe('none');
    expect(css.attrs).toEqual({ theme: 'custom:neon', scheme: 'light', density: 'compact', motion: 'reduce', backdrop: 'off' });
  });

  it('makes the big surfaces translucent over a picture, never the raised ones', () => {
    const t = { ...neon, base: 'dark' as const, colors: {}, background: { path: 'C:/a b/"x".png', dim: 30, opacity: 80 } };
    const css = lookCss(resolveLook({ theme: 'custom', custom_theme: 'neon' }, [t], env, url));
    expect(css.vars['--panel']).toBe('rgb(29 29 29 / 80%)');
    expect(css.vars['--panel-raised']).toBe(themes.dark.tokens.panelRaised);
    expect(css.vars['--input']).toBe(themes.dark.tokens.input);
    expect(css.vars['--backdrop-dim']).toBe('rgb(0 0 0 / 30%)');
    expect(css.vars['--backdrop-image']).toBe('url("asset://C:/a b/%22x%22.png")');
    expect(css.attrs.backdrop).toBe('on');
  });

  it('keeps quotes out of the font name', () => {
    const css = lookCss(resolveLook({ theme: 'custom', custom_theme: 'neon' }, [{ ...neon, font: 'A"B\\C' }], env, url));
    expect(css.vars['--font']?.startsWith('"ABC", ')).toBe(true);
  });

  it("matches app.css's default font and text scale", () => {
    const appCss = readFileSync(new URL('../app.css', import.meta.url), 'utf8');
    const font = /--font: ([^;]+);/.exec(appCss)?.[1]?.replace(/\s+/g, ' ');
    expect(font).toBe(DEFAULT_FONT);
    expect(appCss).toContain('--text-scale: 1;');
    expect(appCss).toContain(":root[data-density='compact']");
    expect(appCss).toContain(":root[data-motion='reduce'] *");
  });
});

describe('colour helpers', () => {
  it('mixes, dims and converts names', () => {
    expect(mix('#000000', '#ffffff', 0.5)).toBe('#808080');
    expect(mix('#102030', '#102030', 0.3)).toBe('#102030');
    expect(dimColour('#ffffff', 'dark', 100)).toBe('#000000');
    expect(dimColour('#000000', 'light', 50)).toBe('#808080');
    expect(tokenKey('panel-raised')).toBe('panelRaised');
    expect(tokenKey('on-accent')).toBe('onAccent');
  });
  it('lists every token key the engine accepts', () => {
    const engine = [
      'gutter', 'chrome', 'statusbar', 'panel', 'panel-raised', 'viewer', 'input', 'border', 'border-strong',
      'text', 'text-strong', 'text-muted', 'accent', 'accent-strong', 'accent-text', 'on-accent', 'good', 'warn',
      'bad', 'rec', 'focus',
    ];
    const ui = Object.keys(themes.dark.tokens).filter((k) => k !== 'hover');
    expect(engine.map(tokenKey).sort()).toEqual([...ui].sort() as (keyof Tokens)[]);
  });
});
