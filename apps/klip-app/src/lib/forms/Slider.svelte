<!-- A horizontal slider with a readout: volumes and the like. Commits on every move. -->
<script lang="ts">
  let {
    value,
    min,
    max,
    step = 0.01,
    label,
    format = (v: number) => String(v),
    disabled = false,
    onchange,
  }: {
    value: number;
    min: number;
    max: number;
    step?: number;
    label: string;
    format?: (v: number) => string;
    disabled?: boolean;
    onchange: (v: number) => void;
  } = $props();

  const pct = $derived(max > min ? ((value - min) / (max - min)) * 100 : 0);
</script>

<label class="slider" class:disabled>
  <span class="name">{label}</span>
  <input
    type="range"
    {min}
    {max}
    {step}
    {value}
    {disabled}
    aria-label={label}
    style:--pct="{pct}%"
    oninput={(e) => onchange(Number((e.currentTarget as HTMLInputElement).value))}
  />
  <span class="mono readout">{format(value)}</span>
</label>

<style>
  .slider {
    display: grid;
    grid-template-columns: 64px minmax(0, 1fr) 48px;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-sm);
  }
  .disabled {
    opacity: 0.5;
  }
  .readout {
    text-align: right;
    color: var(--accent-text);
  }
  input[type='range'] {
    appearance: none;
    width: 100%;
    height: 4px;
    border-radius: 2px;
    background: linear-gradient(to right, var(--accent) var(--pct), var(--panel-raised) var(--pct));
    outline: none;
  }
  input[type='range']::-webkit-slider-thumb {
    appearance: none;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    background: var(--text-strong);
    border: 2px solid var(--panel);
    cursor: pointer;
  }
  input[type='range']:focus-visible::-webkit-slider-thumb {
    box-shadow: 0 0 0 2px var(--focus);
  }
</style>
