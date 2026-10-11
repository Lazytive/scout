<!-- One setting: label on the left, control on the right, hint and error below. -->
<script lang="ts">
  import type { Snippet } from 'svelte';

  let {
    label,
    hint = '',
    error = null,
    children,
  }: { label: string; hint?: string; error?: string | null; children: Snippet } = $props();
</script>

<div class="row">
  <div class="label">{label}</div>
  <div class="control">
    {@render children()}
    {#if error}
      <p class="error" role="alert">{error}</p>
    {:else if hint}
      <p class="hint">{hint}</p>
    {/if}
  </div>
</div>

<style>
  .row {
    display: grid;
    grid-template-columns: 190px minmax(0, 1fr);
    gap: var(--space-4);
    padding: calc(var(--space-2) + 2px) 0;
    border-bottom: 1px solid var(--border);
    align-items: start;
  }
  .row:last-child {
    border-bottom: none;
  }
  .label {
    color: var(--text);
    padding-top: 5px;
  }
  .control {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .control > :global(button) {
    align-self: flex-start;
  }
  .hint {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .error {
    color: var(--bad);
    font-size: var(--text-sm);
  }
</style>
