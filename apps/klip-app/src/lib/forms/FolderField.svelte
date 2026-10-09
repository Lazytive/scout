<!-- A folder: the system's folder picker in the app (a text prompt in the browser). -->
<script lang="ts">
  import { pickFolder } from '../api';

  let {
    value,
    fallback,
    label,
    onchange,
  }: { value: string | null; fallback: string; label: string; onchange: (v: string | null) => void } = $props();

  async function choose() {
    const picked = await pickFolder(value);
    if (picked) onchange(picked);
  }
</script>

<span class="folder">
  <span class="path" class:muted={!value} title={value ?? fallback}>{value ?? fallback}</span>
  <button onclick={choose} aria-label="Choose {label}">Choose…</button>
  {#if value}
    <button class="flat reset" onclick={() => onchange(null)}>Use default</button>
  {/if}
</span>

<style>
  .folder {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    max-width: 100%;
  }
  .path {
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 5px 8px;
    min-width: 0;
    max-width: 420px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
  .reset {
    color: var(--accent-text);
  }
</style>
