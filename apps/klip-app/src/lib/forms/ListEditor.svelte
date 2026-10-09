<!-- A short list of names (programs): chips with a remove button, and a field to add. -->
<script lang="ts">
  let {
    items,
    label,
    placeholder = '',
    onchange,
  }: { items: string[]; label: string; placeholder?: string; onchange: (items: string[]) => void } = $props();

  let draft = $state('');

  function add() {
    const v = draft.trim();
    if (!v) return;
    draft = '';
    if (!items.some((i) => i.toLowerCase() === v.toLowerCase())) onchange([...items, v]);
  }
</script>

<div class="list">
  {#if items.length > 0}
    <ul aria-label={label}>
      {#each items as item (item)}
        <li>
          <span>{item}</span>
          <button
            class="flat x"
            aria-label="Remove {item}"
            onclick={() => onchange(items.filter((i) => i !== item))}>×</button
          >
        </li>
      {/each}
    </ul>
  {/if}
  <span class="add">
    <input
      type="text"
      aria-label="Add to {label}"
      {placeholder}
      bind:value={draft}
      onkeydown={(e) => e.key === 'Enter' && add()}
    />
    <button onclick={add} disabled={!draft.trim()}>Add</button>
  </span>
</div>

<style>
  .list {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  li {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    background: var(--panel-raised);
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 1px 2px 1px 10px;
    font-size: var(--text-sm);
    color: var(--text-strong);
  }
  .x {
    color: var(--text-muted);
    padding: 0 6px;
  }
  .x:hover {
    color: var(--text-strong);
  }
  .add {
    display: inline-flex;
    gap: var(--space-2);
  }
  input {
    font: inherit;
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    height: 28px;
    padding: 0 8px;
    width: 220px;
  }
</style>
