// Appearance (M10, ADR 0019): which look the settings ask for, how the accent colour is
// derived, the contrast check, and putting it all on the document root. Pure functions
// first (tested in appearance.test.ts); the DOM ones at the end.

import {
  contrast,
  cssVar,
  resolve,
  textPairs,
  themes,
  uiPairs,
  type ThemeId,
  type ThemeSetting,
  type Tokens,
} from './themes';

export type Density = 'comfortable' | 'compact';
export type TextSize = 'small' | 'default' | 'large' | 'larger';
export type Motion = 'system' | 'reduce' | 'full';

/** `settings.appearance`. */
export interface Appearance {
  theme: ThemeSetting | 'custom';
  custom_theme: string | null;
  accent: string | null;
  density: Density;
  text_size: TextSize;
  motion: Motion;
}

export const defaultAppearance: Appearance = {
  theme: 'system',
  custom_theme: null,
  accent: null,
  density: 'comfortable',
  text_size: 'default',
  motion: 'system',
};

/** A custom theme as `theme.list` gives it (colour keys are CSS names: `text-muted`). */
export interface CustomTheme {
  id: string;
  name: string;
  base: ThemeId;
  colors: Record<string, string>;
  radius: number;
  font?: string | null;
  background: { path: string; dim: number; opacity: number; version?: number } | null;
}

export const TEXT_SCALE: Record<TextSize, number> = {
  small: 0.9,
  default: 1,
  large: 1.15,
  larger: 1.3,
};

export const RADIUS_DEFAULT = 6;
export const RADIUS_MAX = 16;
/** `app.css`'s `--font`, kept equal by appearance.test.ts. */
export const DEFAULT_FONT =
  "'Segoe UI Variable Text', 'Segoe UI', 'Inter', system-ui, -apple-system, 'Noto Sans', 'Cantarell', sans-serif";

/** Accent swatches offered next to the colour picker. */
export const ACCENT_SWATCHES = ['#1f6fe0', '#7c4dff', '#c2338f', '#cc3a2f', '#e0601f', '#e6b800', '#2e9e5b', '#0d7f89'];

/** Surfaces that turn translucent over a background picture (the picture itself shows
    in the gutters; raised surfaces and inputs stay opaque). */
export const BACKDROP_SURFACES: (keyof Tokens)[] = ['chrome', 'statusbar', 'panel', 'viewer'];

/** Everything the root gets. */
export interface Look {
  /** `data-theme`: a built-in id or `custom:<id>`. */
  id: string;
  scheme: 'dark' | 'light';
  /** Effective opaque colours (hover is translucent by design). */
  tokens: Tokens;
  radius: number;
  font: string | null;
  density: Density;
  textScale: number;
  reduceMotion: boolean;
  /** `url`: what CSS loads (asset URL in the app). */
  background: { url: string; dim: number; opacity: number } | null;
}

// --- colours ------------------------------------------------------------------------

const HEX = /^#[0-9a-f]{6}$/;

export function isHex(s: unknown): s is string {
  return typeof s === 'string' && HEX.test(s);
}

function rgb(hex: string): [number, number, number] {
  return [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16)) as [number, number, number];
}

function hex([r, g, b]: number[]): string {
  return `#${[r, g, b]
    .map((c) =>
      Math.round(Math.min(255, Math.max(0, c ?? 0)))
        .toString(16)
        .padStart(2, '0'),
    )
    .join('')}`;
}

/** `a` moved `t` (0–1) of the way to `b`, in sRGB. */
export function mix(a: string, b: string, t: number): string {
  const [x, y] = [rgb(a), rgb(b)];
  return hex(x.map((c, i) => c + ((y[i] ?? 0) - c) * t));
}

/** `kebab-case` → `camelCase`. */
export function tokenKey(css: string): string {
  return css.replace(/-([a-z])/g, (_, c: string) => c.toUpperCase());
}

/** The first of `candidates` reaching `need` against every background, else the best. */
function firstReadable(candidates: string[], bgs: string[], need: number): string {
  let best = candidates[0] ?? '#ffffff';
  let bestRatio = 0;
  for (const c of candidates) {
    const r = Math.min(...bgs.map((b) => contrast(c, b)));
    if (r >= need) return c;
    if (r > bestRatio) [best, bestRatio] = [c, r];
  }
  return best;
}

/** The theme with `accent` as its accent: buttons use it as is; the pressed shade, the
    text on it, the accent-coloured text and the focus ring are derived so they read at
    AA where any shade of that colour can. */
export function withAccent(tokens: Tokens, scheme: 'dark' | 'light', accent: string): Tokens {
  // White or black text, whichever reads better (one of them always reaches 4.5:1);
  // the pressed shade moves away from it, so the text reads there too.
  const onAccent = contrast('#ffffff', accent) >= contrast('#000000', accent) ? '#ffffff' : '#000000';
  const strong = mix(accent, onAccent === '#ffffff' ? '#000000' : '#ffffff', 0.15);
  const towards = scheme === 'dark' ? '#ffffff' : '#000000';
  const steps = Array.from({ length: 21 }, (_, i) => mix(accent, towards, i / 20));
  const text = firstReadable(steps, [tokens.panel, tokens.viewer, tokens.panelRaised], 4.5);
  return { ...tokens, accent, accentStrong: strong, onAccent, accentText: text, focus: text };
}

// --- resolving ----------------------------------------------------------------------

export interface Env {
  prefersLight: boolean;
  prefersReducedMotion: boolean;
}

const ACCENT_SHADES: (keyof Tokens)[] = ['accentStrong', 'accentText', 'onAccent', 'focus'];

/** A custom theme's colours: its base's, then its own. When it sets an accent, the
    accent's shades are derived from it (like the accent setting) unless it sets them
    too. `hover` is always the base's. */
export function themeTokens(t: CustomTheme): { tokens: Tokens; scheme: 'dark' | 'light' } {
  const base = themes[t.base] ?? themes.dark;
  let tokens: Tokens = { ...base.tokens };
  const own: Partial<Tokens> = {};
  for (const [k, v] of Object.entries(t.colors ?? {})) {
    const key = tokenKey(k) as keyof Tokens;
    if (key in tokens && key !== 'hover' && isHex(v)) own[key] = v;
  }
  tokens = { ...tokens, ...own };
  if (own.accent) {
    const derived = withAccent(tokens, base.scheme, own.accent);
    for (const k of ACCENT_SHADES) if (!own[k]) tokens[k] = derived[k];
  }
  return { tokens, scheme: base.scheme };
}

/** The look `a` asks for. `custom` is the custom themes list; `preview` (the theme
    editor's draft) replaces the custom theme in use. A custom theme that isn't there
    falls back to Follow system. */
export function resolveLook(
  a: Partial<Appearance> | null | undefined,
  custom: CustomTheme[],
  env: Env,
  pictureUrl: (path: string, version?: number) => string,
  preview: CustomTheme | null = null,
): Look {
  const s = { ...defaultAppearance, ...(a ?? {}) };
  const ct = preview ?? (s.theme === 'custom' ? (custom.find((t) => t.id === s.custom_theme) ?? null) : null);
  let id: string;
  let scheme: 'dark' | 'light';
  let tokens: Tokens;
  if (ct) {
    ({ tokens, scheme } = themeTokens(ct));
    id = `custom:${ct.id}`;
  } else {
    const t = resolve(s.theme === 'custom' ? 'system' : s.theme, env.prefersLight);
    ({ id, scheme } = t);
    tokens = { ...t.tokens };
  }
  if (isHex(s.accent)) tokens = withAccent(tokens, scheme, s.accent);
  const radius = ct ? Math.min(RADIUS_MAX, Math.max(0, Math.round(ct.radius ?? RADIUS_DEFAULT))) : RADIUS_DEFAULT;
  return {
    id,
    scheme,
    tokens,
    radius,
    font: ct?.font ? ct.font : null,
    density: s.density === 'compact' ? 'compact' : 'comfortable',
    textScale: TEXT_SCALE[s.text_size] ?? 1,
    reduceMotion: s.motion === 'reduce' || (s.motion !== 'full' && env.prefersReducedMotion),
    background: ct?.background
      ? {
          url: pictureUrl(ct.background.path, ct.background.version),
          dim: ct.background.dim,
          opacity: ct.background.opacity,
        }
      : null,
  };
}

/** The picture's darkest and lightest colours after dimming (sampled in the DOM). */
export interface PictureRange {
  dark: string;
  light: string;
}

/** Dimming: towards black on a dark scheme, white on a light one. */
export function dimColour(c: string, scheme: 'dark' | 'light', dim: number): string {
  return mix(c, scheme === 'dark' ? '#000000' : '#ffffff', dim / 100);
}

// --- contrast check -----------------------------------------------------------------

export const TOKEN_LABELS: Partial<Record<keyof Tokens, string>> = {
  text: 'Text',
  textStrong: 'Bright text',
  textMuted: 'Muted text',
  accentText: 'Accent text',
  onAccent: 'Text on accent buttons',
  good: 'Success text',
  warn: 'Warning text',
  bad: 'Error text',
  borderStrong: 'Outlines',
  focus: 'Focus ring',
  rec: 'Recording dot',
  panel: 'panels',
  chrome: 'the title bar',
  viewer: 'the viewer',
  statusbar: 'the status bar',
  panelRaised: 'raised panels',
  accent: 'the accent colour',
  accentStrong: 'the pressed accent',
};

export interface ContrastProblem {
  fg: keyof Tokens;
  bg: keyof Tokens;
  ratio: number;
  need: number;
  /** "Muted text on panels". */
  label: string;
}

/** Pairs below WCAG AA (text 4.5:1, other parts 3:1). With a picture, a translucent
    surface is checked over the picture's darkest and lightest dimmed colours. */
export function contrastProblems(look: Look, picture: PictureRange | null = null): ContrastProblem[] {
  const t = look.tokens;
  const bgsFor = (bg: keyof Tokens): string[] => {
    if (!look.background || !picture || !BACKDROP_SURFACES.includes(bg)) return [t[bg]];
    const o = look.background.opacity / 100;
    return [picture.dark, picture.light].map((p) => mix(p, t[bg], o));
  };
  const out: ContrastProblem[] = [];
  const check = (pairs: [keyof Tokens, keyof Tokens][], need: number) => {
    for (const [fg, bg] of pairs) {
      const ratio = Math.min(...bgsFor(bg).map((b) => contrast(t[fg], b)));
      if (ratio < need) {
        out.push({
          fg,
          bg,
          ratio,
          need,
          label: `${TOKEN_LABELS[fg] ?? fg} on ${TOKEN_LABELS[bg] ?? bg}`,
        });
      }
    }
  };
  check(textPairs, 4.5);
  check(uiPairs, 3);
  return out;
}

// --- CSS ----------------------------------------------------------------------------

/** A CSS string literal (font names are already restricted by the engine). */
function cssString(s: string): string {
  return `"${s.replace(/["\\\n\r]/g, '')}"`;
}

/** A URL inside `url("…")`. */
function cssUrl(u: string): string {
  return `url("${u.replace(/["\\\n\r]/g, encodeURIComponent)}")`;
}

/** `#rrggbb` at `pct` % opacity. */
function translucent(c: string, pct: number): string {
  const [r, g, b] = rgb(c);
  return `rgb(${r} ${g} ${b} / ${pct}%)`;
}

/** The CSS variables and root attributes for a look. */
export function lookCss(look: Look): { vars: Record<string, string>; attrs: Record<string, string> } {
  const vars: Record<string, string> = {};
  for (const [k, v] of Object.entries(look.tokens)) vars[cssVar(k)] = v;
  const bg = look.background;
  if (bg) {
    for (const k of BACKDROP_SURFACES) vars[cssVar(k)] = translucent(look.tokens[k], bg.opacity);
    vars['--backdrop-image'] = cssUrl(bg.url);
    vars['--backdrop-dim'] = translucent(look.scheme === 'dark' ? '#000000' : '#ffffff', bg.dim);
  } else {
    vars['--backdrop-image'] = 'none';
    vars['--backdrop-dim'] = 'transparent';
  }
  vars['--radius'] = `${look.radius}px`;
  vars['--radius-sm'] = `${Math.round((look.radius * 2) / 3)}px`;
  vars['--font'] = look.font ? `${cssString(look.font)}, ${DEFAULT_FONT}` : DEFAULT_FONT;
  vars['--text-scale'] = String(look.textScale);
  return {
    vars,
    attrs: {
      theme: look.id,
      scheme: look.scheme,
      density: look.density,
      motion: look.reduceMotion ? 'reduce' : 'full',
      backdrop: bg ? 'on' : 'off',
    },
  };
}

// --- DOM ----------------------------------------------------------------------------

const CACHE_KEY = 'klip.look.v1';

function put(css: ReturnType<typeof lookCss>, root: HTMLElement, scheme: string) {
  for (const [k, v] of Object.entries(css.vars)) root.style.setProperty(k, v);
  for (const [k, v] of Object.entries(css.attrs)) root.dataset[k] = v;
  root.style.colorScheme = scheme;
}

/** Put a look on the root and (`remember`) keep it for the next start's first paint. */
export function applyLook(look: Look, root: HTMLElement = document.documentElement, remember = true): void {
  const css = lookCss(look);
  put(css, root, look.scheme);
  if (!remember) return;
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify({ ...css, scheme: look.scheme }));
  } catch {
    // No storage (private mode): the first paint is the default theme.
  }
}

/** Before mounting: the last look, so a custom theme doesn't flash Dark at start. */
export function restoreLook(root: HTMLElement = document.documentElement): void {
  try {
    const raw = localStorage.getItem(CACHE_KEY);
    if (!raw) return;
    const c = JSON.parse(raw) as { vars?: unknown; attrs?: unknown; scheme?: unknown };
    const ok = (o: unknown): o is Record<string, string> =>
      !!o && typeof o === 'object' && Object.values(o).every((v) => typeof v === 'string');
    if (!ok(c.vars) || !ok(c.attrs)) return;
    const vars = Object.fromEntries(Object.entries(c.vars).filter(([k]) => k.startsWith('--')));
    const attrs = Object.fromEntries(Object.entries(c.attrs).filter(([k]) => /^[a-z]+$/.test(k)));
    put({ vars, attrs }, root, c.scheme === 'light' ? 'light' : 'dark');
  } catch {
    // A broken cache only costs the first paint.
  }
}

/** The OS preferences, live. `onChange` runs when either changes; returns a stop. */
export function watchEnv(onChange: () => void): { env: () => Env; stop: () => void } {
  const light = window.matchMedia('(prefers-color-scheme: light)');
  const motion = window.matchMedia('(prefers-reduced-motion: reduce)');
  light.addEventListener('change', onChange);
  motion.addEventListener('change', onChange);
  return {
    env: () => ({ prefersLight: light.matches, prefersReducedMotion: motion.matches }),
    stop: () => {
      light.removeEventListener('change', onChange);
      motion.removeEventListener('change', onChange);
    },
  };
}

/** Samples by scheme, dim and URL (dragging a colour re-checks the contrast). */
const samples = new Map<string, Promise<PictureRange | null>>();

/** A picture's darkest and lightest colours (by luminance), dimmed as shown; `null`
    when it can't be loaded. Cached. */
export function samplePicture(url: string, scheme: 'dark' | 'light', dim: number): Promise<PictureRange | null> {
  const key = `${scheme}|${dim}|${url}`;
  let p = samples.get(key);
  if (!p) {
    if (samples.size >= 16) samples.clear();
    p = sample(url, scheme, dim);
    samples.set(key, p);
  }
  return p;
}

async function sample(url: string, scheme: 'dark' | 'light', dim: number): Promise<PictureRange | null> {
  const img = new Image();
  img.decoding = 'async';
  img.src = url;
  try {
    await img.decode();
  } catch {
    return null;
  }
  const n = 48;
  const canvas = document.createElement('canvas');
  canvas.width = n;
  canvas.height = n;
  const ctx = canvas.getContext('2d', { willReadFrequently: true });
  if (!ctx) return null;
  ctx.drawImage(img, 0, 0, n, n);
  let data: Uint8ClampedArray;
  try {
    data = ctx.getImageData(0, 0, n, n).data;
  } catch {
    return null;
  }
  let lo = { y: Infinity, c: '#000000' };
  let hi = { y: -Infinity, c: '#ffffff' };
  for (let i = 0; i + 2 < data.length; i += 4) {
    const [r, g, b] = [data[i] ?? 0, data[i + 1] ?? 0, data[i + 2] ?? 0];
    const y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    if (y < lo.y) lo = { y, c: hex([r, g, b]) };
    if (y > hi.y) hi = { y, c: hex([r, g, b]) };
  }
  return { dark: dimColour(lo.c, scheme, dim), light: dimColour(hi.c, scheme, dim) };
}
