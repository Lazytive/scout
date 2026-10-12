<!--
  FilmCraft's Project panel for an Advanced edit: the library's other clips as cards
  (hover to scrub). Drag a card onto a video track (or the empty track above them) to put
  it there, or tick cards and Add them after the end of the edit. The bottom bar adds
  files: a sticker (a picture on a video track for a few seconds), a sound effect or
  music (on a sound / music track) at the playhead.
-->
<script lang="ts">
  import { pickMediaFile, thumbUrl } from '../../lib/api';
  import { STICKER_MS, addClip } from '../../lib/advanced';
  import { adoptFile, addToMontage, addToMontageAt, ed, edit } from '../../lib/editor.svelte';
  import { formatDuration } from '../../lib/format';
  import { scrubTile, stripPosition } from '../../lib/library';
  import type { LibClip } from '../../lib/library';
  import { lib } from '../../lib/library.svelte';
  import { pastThreshold } from '../../lib/timeline-view';

  let filter = $state('');
  let picked = $state<string[]>([]);
  let busy = $state(false);
  let note = $state<string | null>(null);
  let scrub = $state<{ key: string; tile: number } | null>(null);
  /** A card being dragged to the timeline. */
  let carry = $state<{ clip: LibClip; x: number; y: number; over: boolean } | null>(null);

  const clips = $derived(
    (lib.listing?.clips ?? [])
      .filter((c) => c.key !== ed.clip?.key && !c.error)
      .filter((c) => !filter || c.name.toLowerCase().includes(filter.toLowerCase())),
  );
  const tiles = $derived(lib.listing?.strip.tiles ?? 0);

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

  // ---- drag a card onto the timeline ----
  /** The video lane under the pointer and the time there, if any. */
  function dropTarget(x: number, y: number): { track: number | 'new'; at: number } | null {
    const lane = document.elementFromPoint(x, y)?.closest('.lane[data-track]') as HTMLElement | null;
    const raw = lane?.dataset.track;
    const pps = Number(lane?.dataset.pps);
    // Not onto a locked track (M9.8.10).
    if (!lane || !raw || !(pps > 0) || lane.dataset.kind !== 'video' || lane.dataset.locked) return null;
    const at = ((x - lane.getBoundingClientRect().left) / pps) * 1000;
    return { track: raw === 'new-video' ? 'new' : Number(raw), at };
  }

  function cardDown(e: PointerEvent, c: LibClip) {
    if (e.button !== 0 || (e.target as HTMLElement).closest('input')) return;
    const x0 = e.clientX;
    const y0 = e.clientY;
    let started = false;
    const move = (m: PointerEvent) => {
      if (!started && !pastThreshold(m.clientX - x0, m.clientY - y0, 6)) return;
      started = true;
      carry = { clip: c, x: m.clientX, y: m.clientY, over: dropTarget(m.clientX, m.clientY) !== null };
    };
    const up = (u: PointerEvent) => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', cancel);
      carry = null;
      if (!started) return;
      const t = dropTarget(u.clientX, u.clientY);
      if (t) void addToMontageAt(c.path, t.at, t.track);
    };
    const cancel = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', cancel);
      carry = null;
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', cancel);
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
      if (edit((p) => (id = addClip(p, 'video', { kind: 'image', path }, 0, STICKER_MS, at)), 'Add a sticker'))
        ed.clipId = id;
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
    if (
      edit(
        (p) => (id = addClip(p, track, { kind: 'file', path }, 0, len, kind === 'music' && ed.playhead === 0 ? 0 : at)),
        kind === 'music' ? 'Add music' : 'Add a sound',
      )
    )
      ed.clipId = id;
  }
</script>

<div class="project">
  <div class="top">
    <span class="muted">{clips.length} {clips.length === 1 ? 'clip' : 'clips'}</span>
    <input class="search" type="search" placeholder="Search" bind:value={filter} aria-label="Filter clips" />
  </div>
  <ul class="cards">
    {#each clips as c (c.key)}
      {@const strip = c.thumbs.strip ? thumbUrl('strip', c.key, c.thumbs.strip) : null}
      {@const poster = c.thumbs.poster ? thumbUrl('poster', c.key, c.thumbs.poster) : null}
      {@const tile = scrub?.key === c.key ? scrub.tile : Math.floor(tiles / 2)}
      <li
        class="card"
        class:picked={picked.includes(c.path)}
        title="{c.name} · drag onto a video track, or tick and Add"
        onpointerdown={(e) => cardDown(e, c)}
        onpointermove={(e) => {
          const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
          scrub = { key: c.key, tile: scrubTile(e.clientX - r.left, r.width, tiles) };
        }}
        onpointerleave={() => (scrub = null)}
      >
        <div
          class="thumb"
          style:background-image={strip && tiles > 0 ? `url('${strip}')` : poster ? `url('${poster}')` : undefined}
          style:background-size={strip && tiles > 0 ? `100% ${tiles * 100}%` : 'cover'}
          style:background-position-y={strip && tiles > 0 ? stripPosition(tile, tiles) : undefined}
        >
          <input
            class="tick"
            type="checkbox"
            checked={picked.includes(c.path)}
            aria-label="Pick {c.name}"
            onchange={() => toggle(c.path)}
          />
        </div>
        <div class="meta">
          <span class="chip" aria-hidden="true"></span>
          <span class="name">{c.name}</span>
          <span class="muted mono">{formatDuration(c.info?.duration_ms ?? 0)}</span>
        </div>
      </li>
    {:else}
      <li class="muted empty">No other clips{filter ? ' match' : ' in the library'}.</li>
    {/each}
  </ul>
  {#if note}<p class="hint muted">{note}</p>{/if}
  <div class="bar">
    <button
      class="flat"
      onclick={() => addFile('sticker')}
      title="A picture over the video for {STICKER_MS / 1000} s, at the playhead">+ Sticker</button
    >
    <button class="flat" onclick={() => addFile('sound')} title="A sound effect at the playhead (it ducks the music)"
      >+ Sound</button
    >
    <button class="flat" onclick={() => addFile('music')} title="Music on its own track (ducked under voices)"
      >+ Music</button
    >
    <span class="grow"></span>
    <button
      class="primary add"
      disabled={picked.length === 0 || busy}
      onclick={addClips}
      title="Add the ticked clips after the end of the edit"
    >
      Add{picked.length ? ` ${picked.length}` : ''}
    </button>
  </div>
</div>

{#if carry}
  <div class="ghost" class:over={carry.over} style:left="{carry.x + 12}px" style:top="{carry.y + 8}px">
    {carry.clip.name}{carry.over ? '' : ' · drop on a video track'}
  </div>
{/if}

<style>
  .project {
    display: flex;
    flex-direction: column;
    min-height: 0;
    gap: 6px;
    padding: var(--space-2) var(--space-2) 6px;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--text-xs);
  }
  .search {
    flex: 1;
    min-width: 0;
    height: 24px;
    padding: 0 8px;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
  }
  .cards {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(118px, 1fr));
    gap: 8px;
    align-content: start;
  }
  .card {
    cursor: grab;
    user-select: none;
    touch-action: none;
    border-radius: var(--radius-sm);
  }
  .card.picked .thumb {
    outline: 2px solid var(--accent-text);
    outline-offset: -2px;
  }
  .thumb {
    position: relative;
    aspect-ratio: 16 / 9;
    background-color: var(--viewer);
    background-repeat: no-repeat;
    border: 1px solid var(--border);
  }
  .tick {
    position: absolute;
    top: 3px;
    left: 3px;
    margin: 0;
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 4px;
    padding-top: 3px;
    font-size: var(--text-xs);
  }
  .chip {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 2px;
    background: #1d4a64;
  }
  .name {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .empty {
    grid-column: 1 / -1;
    font-size: var(--text-sm);
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 2px;
    padding-top: 6px;
    border-top: 1px solid var(--border);
  }
  .bar .flat {
    padding: 0 6px;
    font-size: var(--text-xs);
  }
  .add {
    padding: 0 12px;
  }
  .grow {
    flex: 1;
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
  }
  .ghost {
    position: fixed;
    z-index: 30;
    pointer-events: none;
    max-width: 280px;
    padding: 3px 8px;
    border-radius: var(--radius-sm);
    background: #1d4a64;
    color: #e8eef2;
    font-size: var(--text-xs);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    opacity: 0.85;
    box-shadow: 0 4px 12px rgb(0 0 0 / 40%);
  }
  .ghost.over {
    outline: 2px solid #fff;
  }
  .mono {
    font-family: var(--font-mono);
  }
</style>
