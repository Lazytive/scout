<!-- Minimize, maximize / restore and close at the right end of Scout's own title bar
     (M11.3, Windows), sized and coloured like Windows 11's. -->
<script lang="ts">
  import { onMount } from 'svelte';

  import { closeWindow, minimize, toggleMaximize, watchMaximized } from './window-chrome';

  let maximized = $state(false);

  onMount(() => {
    let stop: (() => void) | undefined;
    let gone = false;
    void watchMaximized((m) => (maximized = m)).then((s) => {
      if (gone) s();
      else stop = s;
    });
    return () => {
      gone = true;
      stop?.();
    };
  });
</script>

<div class="caption" data-no-drag>
  <button class="flat" aria-label="Minimize" title="Minimize" onclick={minimize}>
    <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0 5.5h10" /></svg>
  </button>
  <button class="flat" aria-label={maximized ? 'Restore' : 'Maximize'} title={maximized ? 'Restore Down' : 'Maximize'} onclick={toggleMaximize}>
    {#if maximized}
      <svg viewBox="0 0 10 10" aria-hidden="true"><rect x="0.5" y="2.5" width="7" height="7" /><path d="M2.5 2.5v-2h7v7h-2" /></svg>
    {:else}
      <svg viewBox="0 0 10 10" aria-hidden="true"><rect x="0.5" y="0.5" width="9" height="9" /></svg>
    {/if}
  </button>
  <button class="flat close" aria-label="Close" title="Close" onclick={closeWindow}>
    <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0.5 0.5l9 9M9.5 0.5l-9 9" /></svg>
  </button>
</div>

<style>
  .caption {
    display: flex;
    align-self: stretch;
    flex: none;
  }
  button {
    width: 46px;
    height: 100%;
    min-height: 0;
    padding: 0;
    border: none;
    border-radius: 0;
    display: grid;
    place-items: center;
    color: var(--text-strong);
    background: transparent;
  }
  button:hover:not(:disabled) {
    background: var(--hover);
  }
  button:active:not(:disabled) {
    background: color-mix(in srgb, var(--text-strong) 14%, transparent);
  }
  button.close:hover:not(:disabled) {
    background: #c42b1c;
    color: #fff;
  }
  button.close:active:not(:disabled) {
    background: #b22a1b;
    color: #fff;
  }
  svg {
    width: 10px;
    height: 10px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1;
    shape-rendering: crispEdges;
  }
  .close svg {
    shape-rendering: geometricPrecision;
  }
</style>
