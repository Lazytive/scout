<!-- A hotkey field: click, then press the keys. Esc cancels, Backspace clears. The
     engine's hotkeys are paused meanwhile, or an existing combination would run instead
     of reaching this field. -->
<script lang="ts">
  import { captureKey, keyParts } from '../settings';
  import { pauseHotkeys, resumeHotkeys } from '../store.svelte';

  let { keys, label, onchange }: { keys: string; label: string; onchange: (keys: string) => void } = $props();

  let listening = $state(false);
  let note = $state('');
  let el: HTMLButtonElement | undefined = $state();

  async function start() {
    if (listening) return;
    note = '';
    listening = true;
    await pauseHotkeys();
    el?.focus();
  }

  async function stop() {
    if (!listening) return;
    listening = false;
    await resumeHotkeys();
  }

  function onKey(e: KeyboardEvent) {
    if (!listening) return;
    e.preventDefault();
    e.stopPropagation();
    const c = captureKey(e);
    switch (c.kind) {
      case 'wait':
        return;
      case 'cancel':
        void stop();
        return;
      case 'clear':
        onchange('');
        void stop();
        return;
      case 'unsupported':
        note = `That key (${c.code}) can't be used.`;
        return;
      case 'combo':
        note = c.bare ? 'A key without Ctrl/Alt/Shift/Win is taken from every app while Scout runs.' : '';
        onchange(c.keys);
        void stop();
    }
  }
</script>

<span class="wrap">
  <button
    bind:this={el}
    class="flat capture"
    class:listening
    aria-label={label}
    onclick={start}
    onkeydown={onKey}
    onblur={stop}
  >
    {#if listening}
      <span class="muted">Press the keys…</span>
    {:else if keys}
      {#each keyParts(keys) as k, i (i)}<kbd>{k}</kbd>{/each}
    {:else}
      <span class="muted">Not set</span>
    {/if}
  </button>
  {#if note}<span class="note">{note}</span>{/if}
</span>

<style>
  .wrap {
    display: inline-flex;
    flex-direction: column;
    gap: 4px;
    align-self: flex-start;
  }
  .capture {
    min-width: 170px;
    height: var(--control-h);
    justify-content: flex-start;
    gap: 3px;
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
  }
  .capture:hover {
    border-color: var(--text-muted);
  }
  .listening {
    border-color: var(--focus);
    box-shadow: 0 0 0 1px var(--focus);
  }
  .note {
    color: var(--warn);
    font-size: var(--text-sm);
    max-width: 360px;
  }
</style>
