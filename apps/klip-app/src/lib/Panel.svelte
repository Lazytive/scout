<!-- A docked panel with a tab strip, like the panels in Photocraft / FilmCraft. -->
<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    tabs,
    children,
    actions,
  }: {
    tabs: string[];
    /** Renders the active tab's content. */
    children: Snippet<[string]>;
    /** Extra controls at the right end of the tab strip. */
    actions?: Snippet;
  } = $props();

  let picked = $state('');
  const current = $derived(tabs.includes(picked) ? picked : (tabs[0] ?? ''));
</script>

<section class="panel">
  <header role="tablist">
    {#each tabs as t (t)}
      <button
        class="flat tab"
        class:active={current === t}
        role="tab"
        aria-selected={current === t}
        onclick={() => (picked = t)}
      >
        {t}
      </button>
    {/each}
    <span class="grow"></span>
    {@render actions?.()}
  </header>
  <div class="body" role="tabpanel">
    {@render children(current)}
  </div>
</section>

<style>
  .panel {
    background: var(--panel);
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
  }
  header {
    height: var(--tabs-h);
    display: flex;
    align-items: stretch;
    gap: var(--space-1);
    padding: 0 var(--space-2);
    flex: none;
  }
  .tab {
    color: var(--text);
    border-radius: 0;
    position: relative;
    padding: 0 var(--space-2);
  }
  .tab:hover:not(:disabled) {
    background: transparent;
    color: var(--text-strong);
  }
  .tab.active {
    color: var(--text-strong);
  }
  .tab.active::after {
    content: '';
    position: absolute;
    left: var(--space-2);
    right: var(--space-2);
    bottom: 6px;
    height: 1px;
    background: var(--text-strong);
  }
  .grow {
    flex: 1;
  }
  .body {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: var(--space-2) var(--space-3) var(--space-3);
  }
</style>
