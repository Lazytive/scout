<!-- The library's right panel: the selected clip (name to rename, star, tags, info,
     actions), several selected clips (bulk actions), or the library's totals. -->
<script lang="ts">
  import { thumbUrl } from '../lib/api';
  import { formatBytes, formatDuration, formatTimecode } from '../lib/format';
  import ListEditor from '../lib/forms/ListEditor.svelte';
  import { displayName, qualityLabel } from '../lib/library';
  import {
    askDelete,
    lib,
    openClip,
    renameClip,
    selectedClips,
    setStar,
    setTags,
  } from '../lib/library.svelte';

  let { trashName, renameFocus = 0 }: { trashName: string; renameFocus?: number } = $props();

  const clips = $derived(lib.listing ? selectedClips() : []);
  const one = $derived(clips.length === 1 ? clips[0] : null);
  const total = $derived(clips.reduce((s, c) => s + c.bytes, 0));
  const allStarred = $derived(clips.length > 0 && clips.every((c) => c.starred));

  let draft = $state('');
  let nameInput: HTMLInputElement | undefined = $state();
  let lastKey = '';

  $effect(() => {
    // A new clip selected: its name in the field, the old error gone.
    const k = one?.key ?? '';
    if (k !== lastKey) {
      lastKey = k;
      draft = one?.name ?? '';
      lib.renameError = null;
      lib.tagError = null;
    }
  });

  $effect(() => {
    // F2 in the grid.
    if (renameFocus > 0 && nameInput) {
      nameInput.focus();
      nameInput.select();
    }
  });

  async function commit() {
    if (!one) return;
    if (!draft.trim()) {
      draft = one.name;
      return;
    }
    if (!(await renameClip(one, draft))) nameInput?.focus();
  }

  const rows = $derived.by<[string, string][]>(() => {
    if (!one) return [];
    const i = one.info;
    return [
      ['Game', one.game ?? 'No game folder'],
      ['Date', new Date(one.date_ms).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' })],
      ['Length', i?.duration_ms != null ? formatTimecode(i.duration_ms) : '–'],
      ['Video', i ? [qualityLabel(i), i.codec].filter(Boolean).join(' · ') || '–' : '–'],
      ['Audio tracks', i ? String(i.audio_tracks) : '–'],
      ['Bookmarks', i ? String(i.bookmarks_ms.length) : '–'],
      ['Size', formatBytes(one.bytes)],
    ];
  });
</script>

<div class="details">
  {#if one}
    <div class="preview">
      {#if one.thumbs.poster}
        <img src={thumbUrl('poster', one.key, one.thumbs.poster)} alt="" draggable="false" />
      {/if}
    </div>
    <label class="field">
      <span class="label">Name</span>
      <input
        bind:this={nameInput}
        type="text"
        spellcheck="false"
        aria-invalid={lib.renameError !== null}
        bind:value={draft}
        onkeydown={(e) => {
          if (e.key === 'Enter') void commit();
          if (e.key === 'Escape') {
            draft = one?.name ?? '';
            lib.renameError = null;
            e.currentTarget.blur();
          }
          e.stopPropagation();
        }}
        onblur={() => void commit()}
      />
    </label>
    {#if lib.renameError}<p class="error">{lib.renameError}</p>{/if}
    {#if displayName(one.name) !== one.name}<p class="hint">Shown as “{displayName(one.name)}”.</p>{/if}

    <div class="actions">
      <button class="primary" onclick={() => openClip(one)}>Open</button>
      <button onclick={() => openClip(one, true)}>Show in folder</button>
      <button
        class="flat icon"
        class:on={one.starred}
        aria-pressed={one.starred}
        title={one.starred ? 'Unstar' : 'Star (never removed to stay under the storage cap)'}
        onclick={() => setStar([one], !one.starred)}
      >
        <svg viewBox="0 0 16 16" aria-hidden="true"
          ><path d="M8 1.6l1.9 4 4.4.5-3.3 3 .9 4.3L8 11.2l-3.9 2.2.9-4.3-3.3-3 4.4-.5z" /></svg
        >
        {one.starred ? 'Starred' : 'Star'}
      </button>
    </div>

    <h3>Tags</h3>
    <ListEditor
      items={one.tags}
      label="Tags"
      placeholder="Add a tag"
      onchange={(tags) => setTags(one, tags)}
    />
    {#if lib.tagError}<p class="error">{lib.tagError}</p>{/if}

    <h3>Info</h3>
    <dl>
      {#each rows as [k, v] (k)}
        <dt>{k}</dt>
        <dd class="mono">{v}</dd>
      {/each}
    </dl>
    {#if one.error && !one.info}<p class="note">Klip can't read this file: {one.error}</p>{/if}
    <p class="path" title={one.path}>{one.path}</p>
    <div class="actions end">
      <button class="danger" onclick={() => askDelete([one.key], false)}>Delete…</button>
    </div>
  {:else if clips.length > 1}
    <p class="big">{clips.length} clips selected</p>
    <p class="muted">{formatBytes(total)} · {formatDuration(clips.reduce((s, c) => s + (c.info?.duration_ms ?? 0), 0))}</p>
    <div class="actions column">
      <button onclick={() => setStar(clips, !allStarred)}>{allStarred ? 'Unstar all' : 'Star all'}</button>
      <button class="danger" onclick={() => askDelete(clips.map((c) => c.key), false)}
        >Move {clips.length} clips to the {trashName}…</button
      >
    </div>
  {:else if lib.listing}
    <p class="big">{lib.listing.total} clips</p>
    <dl>
      <dt>Size</dt>
      <dd class="mono">{formatBytes(lib.listing.total_bytes)}</dd>
      <dt>Starred</dt>
      <dd class="mono">{lib.listing.starred}</dd>
      <dt>Games</dt>
      <dd class="mono">{lib.listing.games.filter((g) => g.name).length}</dd>
      {#if lib.listing.thumbs_pending > 0}
        <dt>Thumbnails to make</dt>
        <dd class="mono">{lib.listing.thumbs_pending}</dd>
      {/if}
    </dl>
    <p class="path" title={lib.listing.folder}>{lib.listing.folder}</p>
    <p class="muted hint">Select a clip to rename, tag or open it. Double-click opens it in your player.</p>
  {/if}
</div>

<style>
  .details {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  .preview {
    aspect-ratio: 16 / 9;
    background: var(--viewer);
    border-radius: var(--radius-sm);
    overflow: hidden;
  }
  .preview img {
    width: 100%;
    height: 100%;
    object-fit: contain;
    display: block;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: var(--space-2);
  }
  .label,
  h3 {
    font-size: var(--text-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--text-muted);
  }
  h3 {
    margin: var(--space-3) 0 0;
  }
  input {
    font: inherit;
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    height: 28px;
    padding: 0 8px;
    user-select: text;
  }
  input[aria-invalid='true'] {
    border-color: var(--bad);
  }
  .error {
    color: var(--bad);
    font-size: var(--text-sm);
  }
  .hint {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .actions {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
    margin-top: var(--space-1);
  }
  .actions.column {
    flex-direction: column;
    align-items: flex-start;
  }
  .actions.end {
    margin-top: var(--space-3);
  }
  .icon svg {
    width: 15px;
    height: 15px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
    stroke-linejoin: round;
  }
  .icon.on svg {
    fill: #ffc940;
    stroke: #ffc940;
  }
  .danger {
    border-color: var(--bad);
    color: var(--bad);
  }
  .danger:hover:not(:disabled) {
    border-color: var(--bad);
    background: color-mix(in srgb, var(--bad) 14%, transparent);
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 7px var(--space-4);
    margin: var(--space-1) 0 0;
    font-size: var(--text-sm);
  }
  dt {
    color: var(--text-muted);
  }
  dd {
    margin: 0;
    color: var(--text-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .path {
    margin-top: var(--space-2);
    font-size: var(--text-xs);
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
  .note {
    font-size: var(--text-sm);
    color: var(--warn);
  }
  .big {
    font-size: var(--text-lg);
    color: var(--text-strong);
    margin-top: var(--space-2);
  }
</style>
