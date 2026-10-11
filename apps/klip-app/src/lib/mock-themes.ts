// The browser mock's custom themes (`theme.*`, M10): kept in memory, pictures as data
// URLs, the same rules the engine applies in the ways the UI can trip over.

import { CommandError } from './api';
import { isHex, type CustomTheme } from './appearance';
import type { KlipEvent } from './types';

/** The mock's demo picture (what `pickThemePicture` returns) and an imported theme's. */
const DEMO_PICTURE = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(
  `<svg xmlns="http://www.w3.org/2000/svg" width="1600" height="900" viewBox="0 0 16 9">
    <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#ff7a3d"/><stop offset="0.5" stop-color="#b8326b"/>
      <stop offset="1" stop-color="#2b1a5c"/></linearGradient></defs>
    <rect width="16" height="9" fill="url(#g)"/>
    <circle cx="11" cy="3.2" r="1.6" fill="#ffd27a" opacity="0.85"/></svg>`,
)}`;

const PICTURES: Record<string, string> = {
  'C:/Users/You/Pictures/Klip demo backdrop.png': DEMO_PICTURE,
};

const SUNSET: Omit<CustomTheme, 'id'> = {
  name: 'Sunset',
  base: 'dark',
  colors: { accent: '#e0601f', panel: '#1f1720', chrome: '#241a26', 'panel-raised': '#2e2231' },
  radius: 10,
  font: null,
  background: { path: DEMO_PICTURE, dim: 45, opacity: 82 },
};

function slug(name: string): string {
  return (
    name
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-+|-+$/g, '')
      .slice(0, 32) || 'theme'
  );
}

export function mockThemes(
  emit: (e: KlipEvent) => void,
  onDeleteActive: (id: string) => void,
): (cmd: string, args: unknown) => unknown {
  const themes = new Map<string, CustomTheme>();
  let version = 1;

  const freeId = (name: string) => {
    const base = slug(name);
    for (let n = 1; ; n++) {
      const id = n === 1 ? base : `${base}-${n}`;
      if (!themes.has(id)) return id;
    }
  };
  const changed = (id: string) => emit({ event: 'themes_changed', id });

  return (cmd, raw) => {
    const args = (raw ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case 'theme.list':
        return { dir: 'C:/Users/You/AppData/Roaming/Klip/themes', themes: structuredClone([...themes.values()]), issues: [] };
      case 'theme.save': {
        const t = args.theme as Partial<CustomTheme> | undefined;
        if (!t || typeof t !== 'object') throw new CommandError('bad_args', 'theme: a theme must be a JSON object');
        for (const [k, v] of Object.entries(t.colors ?? {})) {
          if (!isHex(v)) throw new CommandError('bad_args', `theme: colors.${k}: not a #rrggbb colour`);
        }
        const id = typeof args.id === 'string' ? args.id : freeId(t.name ?? 'Custom theme');
        if (typeof args.id === 'string' && !themes.has(id)) throw new CommandError('bad_args', `no theme ${id}`);
        const old = themes.get(id);
        let background = t.background ?? null;
        if ('picture' in args) {
          const p = args.picture as { file?: string } | null;
          if (p === null) background = null;
          else {
            const url = PICTURES[p.file ?? ''] ?? (p.file?.startsWith('data:') ? p.file : undefined);
            if (!url) throw new CommandError('bad_args', "the picture isn't a PNG, JPEG or WebP file");
            background = { path: url, dim: background?.dim ?? 50, opacity: background?.opacity ?? 85, version: ++version };
          }
        } else if (background && old?.background) {
          background = { ...background, path: old.background.path };
        } else {
          background = null;
        }
        const theme: CustomTheme = {
          id,
          name: (t.name ?? '').trim() || 'Custom theme',
          base: t.base ?? 'dark',
          colors: { ...(t.colors ?? {}) },
          radius: Math.min(16, Math.max(0, Math.round(t.radius ?? 6))),
          font: t.font ?? null,
          background,
        };
        themes.set(id, theme);
        changed(id);
        return { id, theme: structuredClone(theme) };
      }
      case 'theme.delete': {
        const id = String(args.id ?? '');
        const deleted = themes.delete(id);
        onDeleteActive(id);
        if (deleted) changed(id);
        return { deleted };
      }
      case 'theme.import': {
        const id = freeId(SUNSET.name);
        const theme = { ...structuredClone(SUNSET), id };
        themes.set(id, theme);
        changed(id);
        return {
          id,
          theme: structuredClone(theme),
          issues: [{ path: 'colors.glow', message: 'unknown colour; ignored' }],
        };
      }
      case 'theme.export':
        if (!themes.has(String(args.id))) throw new CommandError('bad_args', `no theme ${String(args.id)}`);
        return { path: args.path };
      default:
        return undefined;
    }
  };
}
