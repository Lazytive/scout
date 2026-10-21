<!-- Settings → Appearance (M10): theme (built-in or custom), accent colour, text size,
     density, motion and the contrast check; custom themes are made, imported, exported
     and deleted here, and edited in ThemeEditor. -->
<script lang="ts">
  import { ACCENT_SWATCHES, resolveLook, type Appearance, type CustomTheme, type Look } from '../lib/appearance';
  import { pictureUrl } from '../lib/api';
  import Row from '../lib/forms/Row.svelte';
  import Segmented from '../lib/forms/Segmented.svelte';
  import { lib, setWater } from '../lib/library.svelte';
  import type { Settings } from '../lib/settings';
  import {
    app,
    appearance,
    createTheme,
    deleteTheme,
    exportTheme,
    fieldError,
    importTheme,
    setAppearance,
    setTheme,
  } from '../lib/store.svelte';
  import { choices, themes, type ThemeId } from '../lib/themes';
  import ThemeEditor from './ThemeEditor.svelte';

  let { s }: { s: Settings } = $props();

  const a = $derived({ ...appearance(), ...(s.appearance ?? {}) } as Appearance);
  const current = $derived(app.themeSetting);
  const custom = $derived(
    a.theme === 'custom' ? (app.customThemes.find((t) => t.id === a.custom_theme) ?? null) : null,
  );

  let editing = $state<CustomTheme | null>(null);
  let confirmDelete = $state(false);
  let note = $state<{ text: string; details: string[] } | null>(null);

  const env = { prefersLight: false, prefersReducedMotion: false };

  /** What a theme card shows: the theme's own colours (no accent setting). */
  function cardLook(id: string): Look {
    if (id.startsWith('custom:')) {
      const t = app.customThemes.find((c) => `custom:${c.id}` === id);
      return resolveLook({ theme: 'custom', custom_theme: t?.id ?? '' }, t ? [t] : [], env, pictureUrl);
    }
    if (id === 'system') return resolveLook({ theme: 'system' }, [], env, pictureUrl);
    return resolveLook({ theme: id as ThemeId }, [], env, pictureUrl);
  }

  const cards = $derived([
    ...choices.map((c) => ({ id: c.id as string, label: c.label })),
    ...app.customThemes.map((t) => ({ id: `custom:${t.id}`, label: t.name })),
  ]);

  async function newTheme() {
    note = null;
    const base = (app.look?.id && app.look.id in themes ? app.look.id : 'dark') as ThemeId;
    const t = await createTheme(custom ?? { base, name: 'My theme' });
    if (t) editing = t;
  }

  async function doImport() {
    note = null;
    const r = await importTheme();
    if (r) {
      note = {
        text:
          r.issues.length === 0
            ? `Imported “${r.name}”.`
            : `Imported “${r.name}”; ${r.issues.length === 1 ? 'one part was' : `${r.issues.length} parts were`} fixed or left out:`,
        details: r.issues.map((i) => (i.path ? `${i.path}: ${i.message}` : i.message)),
      };
    }
  }

  async function doExport() {
    if (!custom) return;
    note = null;
    const path = await exportTheme(custom);
    if (path) note = { text: `Exported to ${path}`, details: [] };
  }

  async function doDelete() {
    if (!custom) return;
    confirmDelete = false;
    note = null;
    const name = custom.name;
    if (await deleteTheme(custom.id)) note = { text: `Deleted “${name}”.`, details: [] };
  }

  function pickAccent(v: string | null) {
    void setAppearance('accent', v);
  }

  const ratio = (r: number) => `${r.toFixed(1)}:1`;
</script>

{#if editing}
  <ThemeEditor theme={editing} onclose={() => (editing = null)} />
{:else}
  <Row label="Theme">
    <div class="cards" role="radiogroup" aria-label="Theme">
      {#each cards as c (c.id)}
        {@const look = cardLook(c.id)}
        <button
          class="flat card"
          role="radio"
          aria-checked={current === c.id}
          class:on={current === c.id}
          onclick={() => void setTheme(c.id as Parameters<typeof setTheme>[0])}
        >
          <span
            class="mini"
            style:--m-gutter={look.tokens.gutter}
            style:--m-chrome={look.tokens.chrome}
            style:--m-panel={look.tokens.panel}
            style:--m-text={look.tokens.text}
            style:--m-muted={look.tokens.textMuted}
            style:--m-accent={look.tokens.accent}
            style:--m-radius="{Math.min(look.radius, 8)}px"
            style:background-image={look.background ? `url("${look.background.url}")` : undefined}
            aria-hidden="true"
          >
            {#if c.id === 'system'}
              <span class="half"></span>
            {/if}
            <span class="m-bar"></span>
            <span class="m-panel"
              ><span class="m-line"></span><span class="m-line short"></span><span class="m-dot"></span></span
            >
          </span>
          <span class="name">{c.label}</span>
        </button>
      {/each}
    </div>
    <div class="actions">
      <button onclick={newTheme} title={custom ? 'A copy of this theme' : 'A theme based on this one'}>New theme</button
      >
      <button onclick={doImport}>Import…</button>
      {#if custom}
        <button onclick={() => (editing = custom)}>Edit</button>
        <button onclick={doExport}>Export…</button>
        {#if confirmDelete}
          <span class="confirm" role="alert">
            Delete “{custom.name}”?
            <button class="primary destroy" onclick={doDelete}>Delete</button>
            <button onclick={() => (confirmDelete = false)}>Keep</button>
          </span>
        {:else}
          <button onclick={() => (confirmDelete = true)}>Delete</button>
        {/if}
      {/if}
    </div>
    {#if note}
      <div class="note" role="status">
        <p>{note.text}</p>
        {#if note.details.length > 0}
          <ul>
            {#each note.details as d, i (i)}<li>{d}</li>{/each}
          </ul>
        {/if}
      </div>
    {/if}
    {#if app.themeIssues.length > 0}
      <p class="hint">
        {app.themeIssues.length === 1
          ? 'A theme file had a problem'
          : `Theme files had ${app.themeIssues.length} problems`}
        (fixed while loading): {app.themeIssues
          .slice(0, 3)
          .map((i) => `${i.id}${i.path ? ` ${i.path}` : ''}: ${i.message}`)
          .join('; ')}{app.themeIssues.length > 3 ? '; …' : ''}
      </p>
    {/if}
  </Row>

  <Row
    label="Accent colour"
    hint="Buttons, selections and highlights. Text on it switches between white and black to stay readable."
    error={fieldError('appearance.accent')}
  >
    <div class="swatches" role="radiogroup" aria-label="Accent colour">
      <button
        class="flat own"
        role="radio"
        aria-checked={a.accent === null}
        class:on={a.accent === null}
        onclick={() => pickAccent(null)}>Theme's own</button
      >
      {#each ACCENT_SWATCHES as c (c)}
        <button
          class="flat swatch"
          role="radio"
          aria-checked={a.accent === c}
          aria-label={c}
          title={c}
          class:on={a.accent === c}
          style:background={c}
          onclick={() => pickAccent(c)}
        ></button>
      {/each}
      <label class="picker" title="Any colour">
        <input
          type="color"
          aria-label="Any accent colour"
          value={a.accent ?? app.look?.tokens.accent ?? '#1f6fe0'}
          onchange={(e) => pickAccent(e.currentTarget.value)}
        />
        <span class:on={a.accent !== null && !ACCENT_SWATCHES.includes(a.accent)}>Custom…</span>
      </label>
    </div>
  </Row>

  <Row label="Text size">
    <Segmented
      label="Text size"
      value={a.text_size}
      options={[
        { value: 'small', label: 'Small' },
        { value: 'default', label: 'Default' },
        { value: 'large', label: 'Large' },
        { value: 'larger', label: 'Larger' },
      ]}
      onchange={(v) => void setAppearance('text_size', v)}
    />
  </Row>

  <Row label="Density" hint="Compact fits more on screen: tighter spacing, smaller bars and controls.">
    <Segmented
      label="Density"
      value={a.density}
      options={[
        { value: 'comfortable', label: 'Comfortable' },
        { value: 'compact', label: 'Compact' },
      ]}
      onchange={(v) => void setAppearance('density', v)}
    />
  </Row>

  <Row label="Motion" hint="Reduced turns off animations and transitions. System follows Windows' “Animation effects”.">
    <Segmented
      label="Motion"
      value={a.motion}
      options={[
        { value: 'system', label: 'System' },
        { value: 'reduce', label: 'Reduced' },
        { value: 'full', label: 'Full' },
      ]}
      onchange={(v) => void setAppearance('motion', v)}
    />
  </Row>

  <Row
    label="Library background"
    hint="Animated ASCII water behind your clips. It moves only while Scout's window has focus and stays still with reduced motion."
  >
    <Segmented
      label="Library background"
      value={lib.water ? 'water' : 'plain'}
      options={[
        { value: 'plain', label: 'Plain' },
        { value: 'water', label: 'ASCII water' },
      ]}
      onchange={(v) => setWater(v === 'water')}
    />
  </Row>

  <Row label="Contrast">
    {#if app.contrast.length === 0}
      <p class="ok"><span aria-hidden="true">✓</span> All text and controls meet WCAG AA contrast.</p>
    {:else}
      <div class="warn" role="alert">
        <p>
          <span aria-hidden="true">⚠</span>
          Some parts may be hard to read with this {a.accent ? 'theme and accent' : 'theme'}:
        </p>
        <ul>
          {#each app.contrast as p (`${p.fg}/${p.bg}`)}
            <li>{p.label}: <span class="mono">{ratio(p.ratio)}</span> (needs {ratio(p.need)})</li>
          {/each}
        </ul>
      </div>
    {/if}
  </Row>
{/if}

<style>
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(118px, 1fr));
    gap: var(--space-2);
    max-width: 680px;
  }
  .card {
    flex-direction: column;
    align-items: stretch;
    gap: var(--space-1);
    padding: var(--space-1);
    border: 1.5px solid transparent;
    border-radius: var(--radius);
    color: var(--text);
  }
  .card:hover {
    background: var(--hover);
  }
  .card.on {
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .name {
    font-size: var(--text-sm);
    text-align: left;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .mini {
    position: relative;
    display: grid;
    grid-template-rows: 12px 1fr;
    gap: 2px;
    height: 64px;
    padding: 3px;
    border-radius: var(--m-radius);
    background-color: var(--m-gutter);
    background-size: cover;
    background-position: center;
    border: 1px solid var(--border);
    overflow: hidden;
  }
  .half {
    position: absolute;
    inset: 0 0 0 50%;
    background: linear-gradient(#e9e9e9, #c6c6c6);
    opacity: 0.9;
  }
  .m-bar {
    position: relative;
    background: var(--m-chrome);
    border-radius: calc(var(--m-radius) / 2);
  }
  .m-panel {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 6px;
    background: var(--m-panel);
    border-radius: calc(var(--m-radius) / 2);
  }
  .m-line {
    height: 4px;
    width: 70%;
    border-radius: 2px;
    background: var(--m-text);
  }
  .m-line.short {
    width: 45%;
    background: var(--m-muted);
  }
  .m-dot {
    position: absolute;
    right: 6px;
    bottom: 6px;
    width: 16px;
    height: 8px;
    border-radius: 4px;
    background: var(--m-accent);
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    align-items: center;
  }
  .confirm {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--text-strong);
  }
  .destroy {
    background: var(--bad);
    border-color: var(--bad);
  }
  .note {
    color: var(--text);
    font-size: var(--text-sm);
  }
  .note ul,
  .warn ul {
    margin: var(--space-1) 0 0;
    padding-left: var(--space-5);
  }
  .hint {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .swatches {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }
  .own {
    border: 1.5px solid var(--border-strong);
    border-radius: 999px;
    padding: 3px 12px;
    color: var(--text);
  }
  .own.on {
    border-color: var(--accent);
    color: var(--text-strong);
  }
  .swatch {
    width: 24px;
    height: 24px;
    padding: 0;
    border-radius: 50%;
    border: 2px solid transparent;
    box-shadow: inset 0 0 0 1px rgb(0 0 0 / 25%);
  }
  .swatch.on {
    border-color: var(--text-strong);
    box-shadow: inset 0 0 0 2px var(--panel);
  }
  .picker {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    cursor: pointer;
    color: var(--text);
  }
  .picker input {
    width: 28px;
    height: 24px;
    padding: 0;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: none;
    cursor: pointer;
  }
  .picker .on {
    color: var(--accent-text);
  }
  .ok {
    color: var(--good);
  }
  .warn {
    color: var(--warn);
  }
  .warn li {
    color: var(--text);
  }
</style>
