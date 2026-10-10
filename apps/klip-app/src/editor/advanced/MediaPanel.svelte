<!--
  What goes into an Advanced edit: library clips appended to the montage (the engine
  places them after the end of the edit), and files: a sticker (a picture on a video
  track for a few seconds), a sound effect or music (on a sound / music track) at the
  playhead.
-->
<script lang="ts">
  import { pickMediaFile } from '../../lib/api';
  import { STICKER_MS, addClip } from '../../lib/advanced';
  import { adoptFile, addToMontage, ed, edit } from '../../lib/editor.svelte';
  import { formatDuration } from '../../lib/format';
  import { lib } from '../../lib/library.svelte';

  let filter = $state('');
  let picked = $state<string[]>([]);
  let busy = $state(false);
  let note = $state<string | null>(null);

  const clips = $derived(
    (lib.listing?.clips ?? [])
      .filter((c) => c.key !== ed.clip?.key && !c.error)
      .filter((c) => !filter || c.name.toLowerCase().includes(filter.toLowerCase())),
  );

  function toggle(path: string) {
    picked = picked.includes(path) ? picked.filter((p) => p !== path) : [...picked, path];
  }

  async function addClips() {
    if (picked.length === 0) return;
    busy = true;
    // In the order of the list (newest first), as the user sees them.
    const order = clips.map((c) => c.path).filter((p) => picked.includes(p));
    await addToMontage(order);
    picked = [];
    busy = false;
  }

  /** How long a sound file lasts, from its metadata (ms; null when it can't be read). */
  function soundLength(url: string): Promise<number | null> {
    return new Promise((resolve) => {
      const a = new Audio();
      const done = (v: number | null) => {
        a.removeAttribute('src');
        resolve(v);
      };
      const timer = setTimeout(() => done(null), 8000);
      a.preload = 'metadata';
      a.onloadedmetadata = () => {
        clearTimeout(timer);
        done(Number.isFinite(a.duration) && a.duration > 0 ? Math.floor(a.duration * 1000) : null);
      };
      a.onerror = () => {
        clearTimeout(timer);
        done(null);
      };
      a.src = url;
    });
  }

  async function addFile(kind: 'sticker' | 'sound' | 'music') {
    note = null;
    const path = await pickMediaFile(kind === 'sticker' ? 'image' : 'audio');
    if (!path) return;
    const url = await adoptFile(kind === 'sticker' ? 'image' : 'audio', path);
    const at = ed.playhead;
    if (kind === 'sticker') {
      let id: number | null = null;
      if (edit((p) => (id = addClip(p, 'video', { kind: 'image', path }, 0, STICKER_MS, at)), 'Add a sticker')) ed.clipId = id;
      return;
    }
    let len = url ? await soundLength(url) : null;
    if (len === null) {
      // The browser mock has no file to read; the app always does.
      len = 5_000;
      note = url ? 'That file’s length couldn’t be read; it was placed as 5 s.' : null;
    }
    const track = kind === 'music' ? 'music' : 'audio';
    let id: number | null = null;
    if (edit((p) => (id = addClip(p, track, { kind: 'file', path }, 0, len, kind === 'music' && ed.playhead === 0 ? 0 : at)), kind === 'music' ? 'Add music' : 'Add a sound'))
      ed.clipId = id;
  }
</script>

<section>
  <h3>Files</h3>
  <div class="row wrap">
    <button onclick={() => addFile('sticker')} title="A picture over the video for {STICKER_MS / 1000} s, at the playhead">+ Sticker</button>
    <button onclick={() => addFile('sound')} title="A sound effect at the playhead (it ducks the music)">+ Sound</button>
    <button onclick={() => addFile('music')} title="Music on its own track (ducked under voices)">+ Music</button>
  </div>
  {#if note}<p class="hint muted">{note}</p>{/if}
</section>

<section class="clips">
  <h3>Montage</h3>
  <p class="hint muted">Pick clips to add after the end of the edit, in this order.</p>
  <input class="text" type="search" placeholder="Filter clips" bind:value={filter} aria-label="Filter clips" />
  <ul>
    {#each clips as c (c.key)}
      <li>
        <label>
          <input type="checkbox" checked={picked.includes(c.path)} onchange={() => toggle(c.path)} />
          <span class="name" title={c.name}>{c.name}</span>
          <span class="muted mono">{formatDuration(c.info?.duration_ms ?? 0)}</span>
        </label>
      </li>
    {:else}
      <li class="muted">No other clips{filter ? ' match' : ' in the library'}.</li>
    {/each}
  </ul>
  <button class="primary" disabled={picked.length === 0 || busy} onclick={addClips}>
    Add {picked.length || ''} {picked.length === 1 ? 'clip' : 'clips'}
  </button>
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
  .row {
    display: flex;
    gap: var(--space-2);
  }
  .wrap {
    flex-wrap: wrap;
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
  }
  .text {
    font: inherit;
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    height: 28px;
    padding: 0 8px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 260px;
    overflow-y: auto;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  li {
    padding: 3px 6px;
    font-size: var(--text-sm);
  }
  li label {
    display: flex;
    align-items: center;
    gap: 6px;
    cursor: pointer;
  }
  .name {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .mono {
    font-family: var(--font-mono);
  }
</style>
