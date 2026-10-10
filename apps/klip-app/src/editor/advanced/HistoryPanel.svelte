<!--
  Undo history: every step of this session by name (click one to go back to just after
  it, or forward again), and the versions the engine kept on disk (`editor.history`,
  one a minute while editing), each restorable — restoring is itself a step to undo.
-->
<script lang="ts">
  import type { Snapshot } from '../../lib/editor.svelte';
  import { ed, listHistory, redo, restoreHistory, undo } from '../../lib/editor.svelte';
  import { formatBytes } from '../../lib/format';

  let saved = $state<Snapshot[] | null>(null);
  let loading = $state(false);

  async function load() {
    loading = true;
    saved = await listHistory();
    loading = false;
  }

  const when = (ms: number) =>
    new Date(ms).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit', second: '2-digit' });
</script>

<section>
  <h3>This session</h3>
  <ol class="steps">
    {#each ed.redoLabels.map((l, i) => ({ l, i })) as r (`r${r.i}`)}
      <li class="future">
        <button class="flat" title="Redo up to here" onclick={() => redo(ed.redoLabels.length - r.i)}>{r.l}</button>
      </li>
    {/each}
    <li class="now"><span>Now</span></li>
    {#each [...ed.undoLabels].map((l, i) => ({ l, i })).reverse() as u (`u${u.i}`)}
      <li>
        <button class="flat" title="Go back to before this" onclick={() => undo(ed.undoLabels.length - u.i)}>{u.l}</button>
      </li>
    {/each}
    <li class="start"><span class="muted">Opened</span></li>
  </ol>
</section>

<section>
  <h3>Saved versions</h3>
  {#if saved === null}
    <button disabled={loading} onclick={load}>{loading ? 'Looking…' : 'Show saved versions'}</button>
  {:else if saved.length === 0}
    <p class="hint muted">None yet: a version is kept when an edit replaces one at least a minute old.</p>
  {:else}
    <ul class="saved">
      {#each saved as s (s.id)}
        <li>
          <span class="mono">{when(s.saved_unix_ms)}</span>
          <span class="muted">{formatBytes(s.bytes)}</span>
          <button class="flat" onclick={async () => {
            await restoreHistory(s.id);
            await load();
          }}>Restore</button>
        </li>
      {/each}
    </ul>
    <button class="flat" onclick={load}>Refresh</button>
  {/if}
</section>

<style>
  section {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding-bottom: var(--space-3);
    margin-bottom: var(--space-3);
    border-bottom: 1px solid var(--border);
  }
  h3 {
    margin: 0;
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted);
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
  }
  ol,
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
    max-height: 320px;
    overflow-y: auto;
  }
  li {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--text-sm);
  }
  li button {
    flex: 1;
    text-align: left;
    justify-content: flex-start;
  }
  .future button {
    color: var(--text-muted);
    font-style: italic;
  }
  .now span {
    padding: 2px 8px;
    border-left: 2px solid var(--accent);
    color: var(--accent-text);
    font-weight: 600;
  }
  .start span {
    padding: 2px 8px;
  }
  .saved li {
    justify-content: space-between;
  }
  .saved button {
    flex: none;
  }
  .mono {
    font-family: var(--font-mono);
  }
</style>
