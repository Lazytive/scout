<!-- A number with a unit; saved on Enter or when the field loses focus. -->
<script lang="ts">
  let {
    value,
    min,
    max,
    step = 1,
    unit = '',
    label,
    disabled = false,
    onchange,
  }: {
    value: number;
    min: number;
    max: number;
    step?: number;
    unit?: string;
    label: string;
    disabled?: boolean;
    onchange: (v: number) => void;
  } = $props();

  let draft = $state<string | null>(null);
  const shown = $derived(draft ?? String(value));

  function commit() {
    if (draft === null) return;
    const n = Number(draft);
    draft = null;
    if (Number.isFinite(n) && n !== value) onchange(Math.round(n / step) * step);
  }
</script>

<span class="field">
  <input
    type="number"
    aria-label={label}
    {min}
    {max}
    {step}
    {disabled}
    value={shown}
    oninput={(e) => (draft = e.currentTarget.value)}
    onblur={commit}
    onkeydown={(e) => {
      if (e.key === 'Enter') commit();
      if (e.key === 'Escape') draft = null;
    }}
  />
  {#if unit}<span class="unit">{unit}</span>{/if}
</span>

<style>
  .field {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    align-self: flex-start;
  }
  input {
    font: inherit;
    font-family: var(--font-mono);
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    height: 28px;
    width: 96px;
    padding: 0 8px;
    text-align: right;
  }
  .unit {
    color: var(--text-muted);
  }
</style>
