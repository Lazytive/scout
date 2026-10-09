<!-- A checkbox in the Photocraft style: a blue square with a white tick. -->
<script lang="ts">
  let {
    checked,
    label,
    disabled = false,
    onchange,
  }: { checked: boolean; label: string; disabled?: boolean; onchange: (v: boolean) => void } = $props();
</script>

<label class="toggle" class:disabled>
  <input type="checkbox" {checked} {disabled} onchange={(e) => onchange(e.currentTarget.checked)} />
  <span class="box" aria-hidden="true"></span>
  <span>{label}</span>
</label>

<style>
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    cursor: pointer;
    padding: 4px 0;
    position: relative;
  }
  .disabled {
    opacity: 0.45;
    cursor: default;
  }
  input {
    position: absolute;
    opacity: 0;
    width: 1px;
    height: 1px;
  }
  .box {
    width: 16px;
    height: 16px;
    border-radius: 3px;
    border: 1.5px solid var(--border-strong);
    background: var(--input);
    position: relative;
    flex: none;
  }
  input:checked + .box {
    background: var(--accent);
    border-color: var(--accent);
  }
  input:checked + .box::after {
    content: '';
    position: absolute;
    left: 4px;
    top: 1px;
    width: 4px;
    height: 8px;
    border: solid var(--on-accent);
    border-width: 0 2px 2px 0;
    transform: rotate(45deg);
  }
  input:focus-visible + .box {
    outline: 2px solid var(--focus);
    outline-offset: 1px;
  }
</style>
