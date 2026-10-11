<!--
  FilmCraft's (Premiere's) blue "hot text" number: drag sideways to change it (Shift: ten
  times faster), click to type it, arrow keys step it. `onlive` runs while dragging (the
  caller groups the drag into one undo step), `onset` for a typed or stepped value.
-->
<script lang="ts">
  let {
    value,
    min,
    max,
    step,
    decimals = 1,
    unit = '',
    label,
    disabled = false,
    onlive,
    onset,
  }: {
    value: number;
    min: number;
    max: number;
    step: number;
    decimals?: number;
    unit?: string;
    label: string;
    disabled?: boolean;
    onlive: (v: number) => void;
    onset: (v: number) => void;
  } = $props();

  let typing = $state(false);
  let dragging = $state(false);
  const clamp = (v: number) => Math.min(max, Math.max(min, v));
  const round = (v: number) => Number(v.toFixed(decimals));

  function down(e: PointerEvent) {
    if (disabled || e.button !== 0 || typing) return;
    e.preventDefault();
    const x0 = e.clientX;
    const v0 = value;
    let moved = false;
    const move = (m: PointerEvent) => {
      const dx = m.clientX - x0;
      if (!moved && Math.abs(dx) < 3) return;
      moved = true;
      dragging = true;
      onlive(round(clamp(v0 + Math.round(dx) * step * (m.shiftKey ? 10 : 1))));
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up);
      dragging = false;
      if (!moved) typing = true;
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', up);
  }

  function key(e: KeyboardEvent) {
    if (disabled) return;
    if (e.key === 'Enter') typing = true;
    else if (e.key === 'ArrowUp' || e.key === 'ArrowRight' || e.key === 'ArrowDown' || e.key === 'ArrowLeft') {
      e.preventDefault();
      e.stopPropagation();
      const dir = e.key === 'ArrowUp' || e.key === 'ArrowRight' ? 1 : -1;
      onset(round(clamp(value + dir * step * (e.shiftKey ? 10 : 1))));
    }
  }

  function commit(raw: string) {
    typing = false;
    const n = Number(raw.replace(',', '.'));
    if (Number.isFinite(n) && round(clamp(n)) !== round(value)) onset(round(clamp(n)));
  }

  function focus(el: HTMLInputElement) {
    el.focus();
    el.select();
  }
</script>

{#if typing}
  <input
    class="hot-input mono"
    value={value.toFixed(decimals)}
    aria-label={label}
    use:focus
    onkeydown={(e) => {
      e.stopPropagation();
      if (e.key === 'Enter') commit((e.currentTarget as HTMLInputElement).value);
      else if (e.key === 'Escape') typing = false;
    }}
    onblur={(e) => commit((e.currentTarget as HTMLInputElement).value)}
  />
{:else}
  <span
    class="hot mono"
    class:dragging
    class:disabled
    role="spinbutton"
    tabindex={disabled ? -1 : 0}
    aria-label={label}
    aria-valuenow={value}
    aria-valuemin={min}
    aria-valuemax={max}
    title="Drag to change (Shift: faster), click to type"
    onpointerdown={down}
    onkeydown={key}
  >{value.toFixed(decimals)}{unit}</span>
{/if}

<style>
  .hot {
    color: var(--accent-text);
    cursor: ew-resize;
    user-select: none;
    touch-action: none;
    white-space: nowrap;
    border-bottom: 1px dotted transparent;
  }
  .hot:hover,
  .hot.dragging {
    border-bottom-color: var(--accent-text);
  }
  .hot.disabled {
    color: var(--text-muted);
    cursor: default;
  }
  .hot-input {
    width: 64px;
    height: 20px;
    padding: 0 4px;
    font-size: var(--text-sm);
  }
  .mono {
    font-family: var(--font-mono);
  }
</style>
