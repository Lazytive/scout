<!-- The custom theme editor (M10): every change shows at once on the whole app (a preview
     until Save); pictures are saved as soon as they're picked. -->
<script lang="ts">
  import { onDestroy, untrack } from 'svelte';

  import { RADIUS_MAX, isHex, themeTokens, tokenKey, type CustomTheme } from '../lib/appearance';
  import { pickThemePicture, pictureUrl } from '../lib/api';
  import Row from '../lib/forms/Row.svelte';
  import Segmented from '../lib/forms/Segmented.svelte';
  import Slider from '../lib/forms/Slider.svelte';
  import { app, appearance, saveTheme, showTheme } from '../lib/store.svelte';
  import { themes, type ThemeId, type Tokens } from '../lib/themes';

  let { theme, onclose }: { theme: CustomTheme; onclose: () => void } = $props();

  // A copy to edit; the prop is only the starting point.
  // svelte-ignore state_referenced_locally
  let draft = $state<CustomTheme>(structuredClone($state.snapshot(theme)) as CustomTheme);
  let busy = $state(false);

  // Preview every change. showTheme reads the store (the preview included), so it runs
  // untracked: this effect depends on the draft only.
  $effect(() => {
    const snap = $state.snapshot(draft) as CustomTheme;
    untrack(() => {
      app.themePreview = snap;
      showTheme();
    });
  });
  onDestroy(() => {
    app.themePreview = null;
    showTheme();
  });

  const GROUPS: { label: string; tokens: [string, string][] }[] = [
    {
      label: 'Surfaces',
      tokens: [
        ['gutter', 'Gutters'],
        ['chrome', 'Title bar'],
        ['statusbar', 'Status bar'],
        ['panel', 'Panels'],
        ['panel-raised', 'Raised panels, menus'],
        ['viewer', 'Viewer'],
        ['input', 'Inputs'],
      ],
    },
    { label: 'Lines', tokens: [['border', 'Borders'], ['border-strong', 'Outlines']] },
    {
      label: 'Text',
      tokens: [['text', 'Text'], ['text-strong', 'Bright text'], ['text-muted', 'Muted text']],
    },
    {
      label: 'Accent',
      tokens: [
        ['accent', 'Accent'],
        ['accent-strong', 'Pressed accent'],
        ['accent-text', 'Accent text'],
        ['on-accent', 'Text on accent'],
        ['focus', 'Focus ring'],
      ],
    },
    {
      label: 'Status',
      tokens: [['good', 'Success'], ['warn', 'Warning'], ['bad', 'Error'], ['rec', 'Recording']],
    },
  ];

  const base = $derived(themes[draft.base] ?? themes.dark);
  // What each colour is now: the theme's own, its base's, or derived from its accent.
  const effective = $derived(themeTokens(draft).tokens);
  const colour = (k: string) => draft.colors[k] ?? effective[tokenKey(k) as keyof Tokens];

  function setColour(k: string, v: string) {
    if (isHex(v)) draft.colors[k] = v;
  }
  function resetColour(k: string) {
    delete draft.colors[k];
  }

  // Fonts: a name the engine accepts; suggestions are the common ones found on this PC.
  const FONT_RE = /^[\p{L}\p{N} ._-]{1,64}$/u;
  const COMMON_FONTS = [
    'Segoe UI', 'Segoe UI Variable Text', 'Bahnschrift', 'Inter', 'Roboto', 'Noto Sans', 'Ubuntu',
    'Cantarell', 'Arial', 'Verdana', 'Tahoma', 'Trebuchet MS', 'Calibri', 'Candara', 'Corbel',
    'Georgia', 'Cascadia Code', 'Consolas', 'JetBrains Mono',
  ];
  function installed(name: string): boolean {
    const c = document.createElement('canvas').getContext('2d');
    if (!c) return true;
    const sample = 'mmmmmmmmmmlli1WQ@#';
    return ['monospace', 'serif', 'sans-serif'].some((fb) => {
      c.font = `20px ${fb}`;
      const w = c.measureText(sample).width;
      c.font = `20px "${name}", ${fb}`;
      return c.measureText(sample).width !== w;
    });
  }
  const fonts = COMMON_FONTS.filter(installed);
  let fontText = $state(draft.font ?? '');
  const fontOk = $derived(fontText.trim() === '' || FONT_RE.test(fontText.trim()));
  const fontMissing = $derived(fontOk && fontText.trim() !== '' && !installed(fontText.trim()));
  function setFont(v: string) {
    fontText = v;
    const t = v.trim();
    if (t === '') draft.font = null;
    else if (FONT_RE.test(t)) draft.font = t;
  }

  async function picture(file: string | null) {
    busy = true;
    const saved = await saveTheme(draft, file);
    busy = false;
    if (saved) draft.background = saved.background;
  }
  async function choosePicture() {
    const file = await pickThemePicture();
    if (file) await picture(file);
  }

  async function save() {
    busy = true;
    const saved = await saveTheme(draft);
    busy = false;
    if (saved) onclose();
  }

  const accentOverride = $derived(appearance().accent !== null);
  const ratio = (r: number) => `${r.toFixed(1)}:1`;
</script>

<div class="head">
  <h2>Edit theme</h2>
  <span class="grow"></span>
  <button onclick={onclose} disabled={busy}>Cancel</button>
  <button class="primary" onclick={save} disabled={busy || !draft.name.trim()}>Save</button>
</div>

<Row label="Name">
  <input class="text" type="text" maxlength="48" aria-label="Theme name" bind:value={draft.name} />
</Row>

<Row label="Based on" hint="Colours you don't change come from this theme (it also decides light or dark); the accent's shades follow the accent unless you set them.">
  <Segmented
    label="Based on"
    value={draft.base}
    options={(['dark', 'graphite', 'light', 'oled'] as ThemeId[]).map((id) => ({ value: id, label: themes[id].label }))}
    onchange={(v) => (draft.base = v)}
  />
</Row>

<Row label="Colours" hint={accentOverride ? 'Your accent colour setting is used instead of the Accent group: pick “Theme\'s own” under Accent colour to see these.' : ''}>
  <div class="groups">
    {#each GROUPS as g (g.label)}
      <fieldset>
        <legend>{g.label}</legend>
        {#each g.tokens as [k, label] (k)}
          <div class="tok">
            <input
              type="color"
              aria-label={label}
              value={colour(k)}
              oninput={(e) => setColour(k, e.currentTarget.value)}
            />
            <span class="tname" class:changed={k in draft.colors}>{label}</span>
            <span class="mono hex">{colour(k)}</span>
            {#if k in draft.colors}
              <button class="flat reset" title="Back to {base.label}'s colour" aria-label="Reset {label}" onclick={() => resetColour(k)}>↺</button>
            {/if}
          </div>
        {/each}
      </fieldset>
    {/each}
  </div>
</Row>

<Row label="Corners">
  <Slider
    label="Roundness"
    value={draft.radius}
    min={0}
    max={RADIUS_MAX}
    step={1}
    format={(v) => `${v} px`}
    onchange={(v) => (draft.radius = Math.round(v))}
  />
</Row>

<Row
  label="Font"
  hint={fontMissing ? 'Not installed on this PC: the default font is used.' : 'A font installed on your PC; empty for the default. Fonts are not part of theme files: whoever imports this theme needs it too.'}
  error={fontOk ? null : 'Letters, digits, spaces, . _ and - only (up to 64).'}
>
  <input
    class="text"
    type="text"
    list="klip-fonts"
    placeholder="Default (Segoe UI)"
    aria-label="Font"
    value={fontText}
    oninput={(e) => setFont(e.currentTarget.value)}
  />
  <datalist id="klip-fonts">
    {#each fonts as f (f)}<option value={f}></option>{/each}
  </datalist>
</Row>

<Row label="Background picture" hint="Shown behind the panels (PNG, JPEG or WebP up to 8 MB). Saved as soon as you pick it.">
  {#if draft.background}
    <div class="pic">
      <span class="thumb" style:background-image={`url("${pictureUrl(draft.background.path, draft.background.version)}")`}></span>
      <button onclick={choosePicture} disabled={busy}>Change…</button>
      <button onclick={() => picture(null)} disabled={busy}>Remove</button>
    </div>
    <Slider
      label="Dim"
      value={draft.background.dim}
      min={0}
      max={90}
      step={1}
      format={(v) => `${v} %`}
      onchange={(v) => draft.background && (draft.background.dim = Math.round(v))}
    />
    <Slider
      label="Panel opacity"
      value={draft.background.opacity}
      min={60}
      max={100}
      step={1}
      format={(v) => `${v} %`}
      onchange={(v) => draft.background && (draft.background.opacity = Math.round(v))}
    />
  {:else}
    <button onclick={choosePicture} disabled={busy}>Choose a picture…</button>
  {/if}
</Row>

<Row label="Contrast">
  {#if app.contrast.length === 0}
    <p class="ok"><span aria-hidden="true">✓</span> All text and controls meet WCAG AA contrast.</p>
  {:else}
    <div class="warn" role="alert">
      <p><span aria-hidden="true">⚠</span> Some parts may be hard to read:</p>
      <ul>
        {#each app.contrast as p (`${p.fg}/${p.bg}`)}
          <li>{p.label}: <span class="mono">{ratio(p.ratio)}</span> (needs {ratio(p.need)})</li>
        {/each}
      </ul>
    </div>
  {/if}
</Row>

<style>
  .head {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding-bottom: var(--space-2);
    border-bottom: 1px solid var(--border);
  }
  h2 {
    font-size: var(--text-lg);
    color: var(--text-strong);
    font-weight: 600;
  }
  .grow {
    flex: 1;
  }
  .text {
    font: inherit;
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    height: var(--control-h);
    padding: 0 8px;
    max-width: 320px;
    user-select: text;
  }
  .groups {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(230px, 1fr));
    gap: var(--space-3);
  }
  fieldset {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    margin: 0;
    padding: var(--space-2);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  legend {
    color: var(--text-muted);
    font-size: var(--text-sm);
    padding: 0 4px;
  }
  .tok {
    display: grid;
    grid-template-columns: 28px 1fr auto 20px;
    align-items: center;
    gap: var(--space-2);
    min-height: 26px;
  }
  .tok input {
    width: 28px;
    height: 20px;
    padding: 0;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: none;
    cursor: pointer;
  }
  .tname {
    color: var(--text);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tname.changed {
    color: var(--text-strong);
    font-weight: 600;
  }
  .hex {
    color: var(--text-muted);
    font-size: var(--text-xs);
  }
  .reset {
    color: var(--text-muted);
    padding: 0;
    justify-content: center;
  }
  .reset:hover {
    color: var(--text-strong);
  }
  .pic {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .thumb {
    width: 96px;
    height: 54px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--border);
    background-size: cover;
    background-position: center;
  }
  .ok {
    color: var(--good);
  }
  .warn {
    color: var(--warn);
  }
  .warn ul {
    margin: var(--space-1) 0 0;
    padding-left: var(--space-5);
  }
  .warn li {
    color: var(--text);
  }
</style>
