<!-- The header's theme button (a sun, as in Photocraft) and its menu: the built-in and
     custom themes, and the Appearance settings. -->
<script lang="ts">
  import { app, setTheme } from './store.svelte';
  import { choices } from './themes';

  let open = $state(false);
  let root: HTMLElement | undefined = $state();

  const current = $derived(app.themeSetting);
  const items = $derived([
    ...choices.map((c) => ({ id: c.id as Parameters<typeof setTheme>[0], label: c.label })),
    ...app.customThemes.map((t) => ({ id: `custom:${t.id}` as const, label: t.name })),
  ]);

  function pick(id: Parameters<typeof setTheme>[0]) {
    open = false;
    void setTheme(id);
  }

  function settings() {
    open = false;
    app.settingsPage = 'appearance';
    app.page = 'settings';
  }

  function onWindowClick(e: MouseEvent) {
    if (open && root && !root.contains(e.target as Node)) open = false;
  }

  function onKey(e: KeyboardEvent) {
    if (open && e.key === 'Escape') open = false;
  }
</script>

<svelte:window onclick={onWindowClick} onkeydown={onKey} />

<div class="theme" bind:this={root}>
  <button
    class="flat icon"
    title="Theme"
    aria-label="Theme"
    aria-haspopup="menu"
    aria-expanded={open}
    onclick={() => (open = !open)}
  >
    <svg viewBox="0 0 20 20" width="18" height="18" aria-hidden="true">
      <circle cx="10" cy="10" r="3.6" fill="none" stroke="currentColor" stroke-width="1.5" />
      {#each [0, 45, 90, 135, 180, 225, 270, 315] as a (a)}
        <line
          x1="10"
          y1="2.2"
          x2="10"
          y2="4.2"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          transform="rotate({a} 10 10)"
        />
      {/each}
    </svg>
  </button>
  {#if open}
    <div class="menu" role="menu">
      {#each items as c, i (c.id)}
        {#if i === choices.length}<div class="sep" role="separator"></div>{/if}
        <button
          class="flat item"
          role="menuitemradio"
          aria-checked={current === c.id}
          onclick={() => pick(c.id)}
        >
          <span class="check" aria-hidden="true">{current === c.id ? '✓' : ''}</span>
          <span class="label">{c.label}</span>
        </button>
      {/each}
      <div class="sep" role="separator"></div>
      <button class="flat item" role="menuitem" onclick={settings}>
        <span class="check" aria-hidden="true"></span>
        Appearance settings…
      </button>
    </div>
  {/if}
</div>

<style>
  .theme {
    position: relative;
  }
  .icon {
    color: var(--text);
    width: 30px;
    height: 30px;
    justify-content: center;
    padding: 0;
  }
  .icon:hover {
    color: var(--text-strong);
  }
  .menu {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    min-width: 170px;
    max-width: 260px;
    background: var(--panel-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 8px 24px rgb(0 0 0 / 35%);
    padding: var(--space-1);
    display: flex;
    flex-direction: column;
    z-index: 10;
  }
  .item {
    color: var(--text-strong);
    justify-content: flex-start;
    padding: 6px var(--space-2);
    gap: var(--space-2);
  }
  .item:hover {
    background: var(--hover);
  }
  .check {
    width: 14px;
    flex: none;
    color: var(--accent-text);
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sep {
    height: 1px;
    margin: var(--space-1) 0;
    background: var(--border);
  }
</style>
