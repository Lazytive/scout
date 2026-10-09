<!-- One of a few choices, as a row of joined pills (a radio group). -->
<script lang="ts" generics="T extends string">
  let {
    value,
    options,
    label,
    onchange,
  }: { value: T; options: { value: T; label: string }[]; label: string; onchange: (v: T) => void } = $props();
</script>

<div class="seg" role="radiogroup" aria-label={label}>
  {#each options as o (o.value)}
    <button
      class="flat"
      role="radio"
      aria-checked={value === o.value}
      class:on={value === o.value}
      onclick={() => onchange(o.value)}
    >
      {o.label}
    </button>
  {/each}
</div>

<style>
  .seg {
    display: inline-flex;
    align-self: flex-start;
    border: 1.5px solid var(--border-strong);
    border-radius: 999px;
    padding: 2px;
    gap: 2px;
    flex-wrap: wrap;
  }
  button {
    color: var(--text);
    border-radius: 999px;
    padding: 3px 14px;
  }
  button:hover:not(.on) {
    background: var(--hover);
    color: var(--text-strong);
  }
  .on {
    background: var(--accent);
    color: var(--on-accent);
  }
</style>
