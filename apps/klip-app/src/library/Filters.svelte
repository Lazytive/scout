<!-- The library's left panel: all / starred, date range, games and tags, with counts. -->
<script lang="ts">
  import type { DateRange } from '../lib/library';
  import { lib, setQuery } from '../lib/library.svelte';

  const listing = $derived(lib.listing);
  const q = $derived(lib.query);

  const ranges: { id: DateRange; label: string }[] = [
    { id: 'all', label: 'Any time' },
    { id: 'today', label: 'Today' },
    { id: 'week', label: 'Last 7 days' },
    { id: 'month', label: 'Last 30 days' },
  ];
</script>

<nav class="filters" aria-label="Filters">
  <ul>
    <li>
      <button
        class="flat row"
        class:on={!q.starred}
        aria-pressed={!q.starred}
        onclick={() => setQuery({ starred: false })}
      >
        <span>All clips</span><span class="n mono">{listing?.total ?? ''}</span>
      </button>
    </li>
    <li>
      <button
        class="flat row"
        class:on={q.starred}
        aria-pressed={q.starred}
        onclick={() => setQuery({ starred: !q.starred })}
      >
        <span>Starred</span><span class="n mono">{listing?.starred ?? ''}</span>
      </button>
    </li>
  </ul>

  <h3>Date</h3>
  <ul>
    {#each ranges as r (r.id)}
      <li>
        <button
          class="flat row"
          class:on={q.range === r.id}
          aria-pressed={q.range === r.id}
          onclick={() => setQuery({ range: r.id })}
        >
          <span>{r.label}</span>
        </button>
      </li>
    {/each}
  </ul>

  <h3>Games</h3>
  <ul>
    <li>
      <button
        class="flat row"
        class:on={q.game === null}
        aria-pressed={q.game === null}
        onclick={() => setQuery({ game: null })}
      >
        <span>All games</span>
      </button>
    </li>
    {#each listing?.games ?? [] as g (g.name)}
      <li>
        <button
          class="flat row"
          class:on={q.game === g.name}
          aria-pressed={q.game === g.name}
          title={g.name || 'Clips not in a game folder'}
          onclick={() => setQuery({ game: q.game === g.name ? null : g.name })}
        >
          <span class="label">{g.name || 'No game'}</span><span class="n mono">{g.count}</span>
        </button>
      </li>
    {/each}
  </ul>

  {#if (listing?.tags.length ?? 0) > 0}
    <h3>Tags</h3>
    <ul class="tags">
      {#each listing?.tags ?? [] as t (t.name)}
        <li>
          <button
            class="flat chip"
            class:on={q.tag?.toLowerCase() === t.name.toLowerCase()}
            aria-pressed={q.tag?.toLowerCase() === t.name.toLowerCase()}
            onclick={() =>
              setQuery({ tag: q.tag?.toLowerCase() === t.name.toLowerCase() ? null : t.name })}
          >
            {t.name}<span class="n mono">{t.count}</span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}
</nav>

<style>
  .filters {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  h3 {
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-muted);
    margin: var(--space-4) 0 var(--space-1) var(--space-2);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .row {
    width: 100%;
    justify-content: space-between;
    color: var(--text);
    padding: 5px var(--space-2);
    min-width: 0;
  }
  .row:hover:not(.on) {
    color: var(--text-strong);
  }
  .row.on {
    background: var(--panel-raised);
    color: var(--text-strong);
    box-shadow: inset 2px 0 0 var(--accent);
  }
  .label {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .n {
    color: var(--text-muted);
    font-size: var(--text-xs);
    flex: none;
  }
  .tags {
    flex-direction: row;
    flex-wrap: wrap;
    gap: 5px;
    padding: 0 var(--space-1);
  }
  .chip {
    border: 1px solid var(--border-strong);
    border-radius: 999px;
    padding: 2px 9px;
    color: var(--text);
    font-size: var(--text-sm);
    gap: 6px;
  }
  .chip.on {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--on-accent);
  }
  .chip.on .n {
    color: var(--on-accent);
  }
</style>
