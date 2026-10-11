<!-- A dropdown (a styled native select: keyboard and screen readers work as usual). -->
<script lang="ts" generics="T extends string | number">
  let {
    value,
    options,
    label,
    disabled = false,
    onchange,
  }: {
    value: T;
    options: { value: T; label: string }[];
    label: string;
    disabled?: boolean;
    onchange: (v: T) => void;
  } = $props();

  function pick(raw: string) {
    const found = options.find((o) => String(o.value) === raw);
    if (found) onchange(found.value);
  }
</script>

<select
  aria-label={label}
  {disabled}
  value={String(value)}
  onchange={(e) => pick(e.currentTarget.value)}
>
  {#each options as o (o.value)}
    <option value={String(o.value)}>{o.label}</option>
  {/each}
</select>

<style>
  select {
    font: inherit;
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    height: var(--control-h);
    padding: 0 28px 0 10px;
    min-width: 180px;
    max-width: 100%;
    align-self: flex-start;
    appearance: none;
    background-image: linear-gradient(45deg, transparent 50%, var(--text) 50%),
      linear-gradient(135deg, var(--text) 50%, transparent 50%);
    background-position:
      calc(100% - 15px) 50%,
      calc(100% - 10px) 50%;
    background-size: 5px 5px;
    background-repeat: no-repeat;
  }
  select:disabled {
    opacity: 0.45;
  }
  option {
    background: var(--panel-raised);
    color: var(--text-strong);
  }
</style>
