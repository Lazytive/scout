// Built-in themes (M6.7): colour tokens only; sizes, spacing and fonts stay in app.css.
// Dark follows FilmCraft, Graphite follows Photocraft (the maintainer's other apps),
// Light is the same layout on light greys, OLED is Dark on pure black. `app.css` holds
// Dark as the first-paint default; a test keeps the two in step, and another checks
// every text/background pair the UI uses against WCAG AA.

export type ThemeId = 'dark' | 'graphite' | 'light' | 'oled';
export type ThemeSetting = ThemeId | 'system';

export interface Tokens {
  gutter: string;
  chrome: string;
  statusbar: string;
  panel: string;
  panelRaised: string;
  viewer: string;
  input: string;
  border: string;
  borderStrong: string;
  hover: string;
  text: string;
  textStrong: string;
  textMuted: string;
  accent: string;
  accentStrong: string;
  accentText: string;
  onAccent: string;
  good: string;
  warn: string;
  bad: string;
  rec: string;
  focus: string;
}

export interface Theme {
  id: ThemeId;
  label: string;
  scheme: 'dark' | 'light';
  tokens: Tokens;
}

const dark: Tokens = {
  gutter: '#0c0c0c',
  chrome: '#1d1d1d',
  statusbar: '#111111',
  panel: '#1d1d1d',
  panelRaised: '#262626',
  viewer: '#0e0e0e',
  input: '#0e0e0e',
  border: '#2e2e2e',
  borderStrong: '#767676',
  hover: 'rgb(255 255 255 / 7%)',
  text: '#d2d2d2',
  textStrong: '#f2f2f2',
  textMuted: '#929292',
  accent: '#1f6fe0',
  accentStrong: '#1a5fc4',
  accentText: '#5c9fff',
  onAccent: '#ffffff',
  good: '#3fbf7f',
  warn: '#e8913a',
  bad: '#f0555b',
  rec: '#ff4b4b',
  focus: '#5c9fff',
};

export const themes: Record<ThemeId, Theme> = {
  dark: { id: 'dark', label: 'Dark', scheme: 'dark', tokens: dark },
  graphite: {
    id: 'graphite',
    label: 'Graphite',
    scheme: 'dark',
    tokens: {
      ...dark,
      gutter: '#1e1e1e',
      chrome: '#323232',
      statusbar: '#262626',
      panel: '#323232',
      panelRaised: '#3d3d3d',
      viewer: '#282828',
      input: '#212121',
      border: '#424242',
      borderStrong: '#8a8a8a',
      hover: 'rgb(255 255 255 / 8%)',
      text: '#d4d4d4',
      textStrong: '#f0f0f0',
      textMuted: '#a3a3a3',
      accentText: '#6fb0ff',
      good: '#4cc98a',
      warn: '#f0a050',
      bad: '#ff7a7f',
      focus: '#6fb0ff',
    },
  },
  light: {
    id: 'light',
    label: 'Light',
    scheme: 'light',
    tokens: {
      gutter: '#c6c6c6',
      chrome: '#f2f2f2',
      statusbar: '#e6e6e6',
      panel: '#f7f7f7',
      panelRaised: '#e3e3e3',
      viewer: '#e9e9e9',
      input: '#ffffff',
      border: '#d6d6d6',
      borderStrong: '#858585',
      hover: 'rgb(0 0 0 / 6%)',
      text: '#2b2b2b',
      textStrong: '#111111',
      textMuted: '#666666',
      accent: '#1f6fe0',
      accentStrong: '#1a5fc4',
      accentText: '#0b5cc7',
      onAccent: '#ffffff',
      good: '#13784f',
      warn: '#964a00',
      bad: '#c9252d',
      rec: '#e02b2b',
      focus: '#1f6fe0',
    },
  },
  oled: {
    id: 'oled',
    label: 'OLED black',
    scheme: 'dark',
    tokens: {
      ...dark,
      gutter: '#000000',
      chrome: '#000000',
      statusbar: '#000000',
      panel: '#000000',
      panelRaised: '#141414',
      viewer: '#000000',
      input: '#0a0a0a',
      border: '#262626',
      borderStrong: '#6e6e6e',
    },
  },
};

/** The menu order. */
export const choices: { id: ThemeSetting; label: string }[] = [
  { id: 'system', label: 'Follow system' },
  { id: 'dark', label: 'Dark' },
  { id: 'graphite', label: 'Graphite' },
  { id: 'light', label: 'Light' },
  { id: 'oled', label: 'OLED black' },
];

/** `textMuted` → `--text-muted`. */
export function cssVar(token: string): string {
  return `--${token.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}`;
}

/** The theme a setting means now. `system` (also the default when unset) follows the OS:
    light → Light, else Dark. */
export function resolve(setting: string | null | undefined, prefersLight: boolean): Theme {
  if (setting === 'system' || setting === null || setting === undefined) {
    return prefersLight ? themes.light : themes.dark;
  }
  return themes[setting as ThemeId] ?? themes.dark;
}

/** Put a theme's tokens on the document root. */
export function applyTheme(theme: Theme, root: HTMLElement = document.documentElement): void {
  for (const [k, v] of Object.entries(theme.tokens)) root.style.setProperty(cssVar(k), v);
  root.style.colorScheme = theme.scheme;
  root.dataset.theme = theme.id;
}

// --- WCAG 2.x contrast -------------------------------------------------------------

function channel(c: number): number {
  const s = c / 255;
  return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
}

/** Relative luminance of `#rrggbb`. */
export function luminance(hex: string): number {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  if (!m) throw new Error(`not a #rrggbb colour: ${hex}`);
  const [r, g, b] = [m[1], m[2], m[3]].map((h) => channel(parseInt(h ?? '0', 16)));
  return 0.2126 * (r ?? 0) + 0.7152 * (g ?? 0) + 0.0722 * (b ?? 0);
}

export function contrast(a: string, b: string): number {
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

/** Text on background pairs the UI uses (AA: 4.5:1). */
export const textPairs: [keyof Tokens, keyof Tokens][] = [
  ['text', 'panel'],
  ['text', 'chrome'],
  ['textStrong', 'panel'],
  ['textStrong', 'chrome'],
  ['textStrong', 'viewer'],
  ['textStrong', 'panelRaised'],
  ['textMuted', 'panel'],
  ['textMuted', 'viewer'],
  ['textMuted', 'statusbar'],
  ['accentText', 'viewer'],
  ['accentText', 'panel'],
  ['onAccent', 'accent'],
  ['onAccent', 'accentStrong'],
  ['good', 'panel'],
  ['warn', 'panel'],
  ['warn', 'statusbar'],
  ['bad', 'panel'],
];

/** Non-text parts that must stand out from their background (AA: 3:1). */
export const uiPairs: [keyof Tokens, keyof Tokens][] = [
  ['borderStrong', 'panel'],
  ['focus', 'panel'],
  ['focus', 'viewer'],
  ['rec', 'viewer'],
  ['good', 'viewer'],
];
