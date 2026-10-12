<!-- One clip in the library grid: its poster, and on hover the strip frame under the
     pointer (hover-scrub) with a playhead; duration badge, star, name, game and time. -->
<script lang="ts">
  import { onMount } from 'svelte';

  import { thumbUrl } from '../lib/api';
  import { formatBytes, formatTimecode } from '../lib/format';
  import { displayName, fitBox, qualityLabel, scrubTile, stripPosition, tileTime, type LibClip } from '../lib/library';
  import { wantThumbs } from '../lib/thumb-requests';

  let {
    clip,
    tiles,
    selected,
    focused,
    showDay,
    onpick,
    onopen,
    onmenu,
    onstar,
    ondragout,
    progress = null,
  }: {
    clip: LibClip;
    tiles: number;
    selected: boolean;
    focused: boolean;
    /** Show the date, not just the time (no day headings above). */
    showDay: boolean;
    onpick: (e: MouseEvent) => void;
    onopen: () => void;
    onmenu: (e: MouseEvent) => void;
    onstar: () => void;
    /** Dragged out of the window as a file (M11.4). */
    ondragout: () => void;
    /** A job on this clip (the Discord copy), 0–100. */
    progress?: number | null;
  } = $props();

  let el: HTMLElement | undefined = $state();
  let hover = $state(false);
  let tile = $state(0);
  let x = $state(0);
  let stripLoaded = $state<string | null>(null);

  const duration = $derived(clip.info?.duration_ms ?? null);
  const fit = $derived(fitBox(clip.info?.width ?? 0, clip.info?.height ?? 0));
  const poster = $derived(clip.thumbs.poster ? thumbUrl('poster', clip.key, clip.thumbs.poster) : null);
  const strip = $derived(clip.thumbs.strip ? thumbUrl('strip', clip.key, clip.thumbs.strip) : null);
  const scrubbing = $derived(hover && strip !== null && stripLoaded === strip);
  const time = $derived(
    new Date(clip.date_ms).toLocaleString([], showDay
      ? { day: 'numeric', month: 'short', hour: '2-digit', minute: '2-digit' }
      : { hour: '2-digit', minute: '2-digit' }),
  );
  const title = $derived(displayName(clip.name));
  // A title made from the clip's time already says when: show its quality instead.
  const timeInTitle = $derived(title !== clip.name && !showDay);
  const detail = $derived(timeInTitle ? qualityLabel(clip.info) || formatBytes(clip.bytes) : time);

  function enter() {
    hover = true;
    if (strip && stripLoaded !== strip) {
      const img = new Image();
      const url = strip;
      img.onload = () => (stripLoaded = url);
      img.src = url;
    }
  }

  function move(e: PointerEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    x = Math.min(r.width, Math.max(0, e.clientX - r.left));
    tile = scrubTile(x, r.width, tiles);
  }

  onMount(() => {
    if (!el) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((en) => en.isIntersecting) && (!clip.thumbs.poster || !clip.thumbs.strip) && !clip.error) {
          wantThumbs(clip.key);
        }
      },
      { rootMargin: '200px' },
    );
    io.observe(el);
    return () => io.disconnect();
  });

  $effect(() => {
    if (focused) el?.scrollIntoView({ block: 'nearest' });
  });
</script>

<div
  bind:this={el}
  class="card"
  class:selected
  class:focused
  role="option"
  aria-selected={selected}
  tabindex="-1"
  data-key={clip.key}
  title={clip.name}
  onclick={onpick}
  ondblclick={onopen}
  oncontextmenu={onmenu}
  onkeydown={() => undefined}
  draggable="true"
  ondragstart={(e) => {
    // The OS drag (a real file) replaces the page's own.
    e.preventDefault();
    hover = false;
    ondragout();
  }}
>
  <div
    class="thumb"
    role="presentation"
    onpointerenter={enter}
    onpointerleave={() => (hover = false)}
    onpointermove={move}
  >
    <div class="frame" style:width="{fit.w}%" style:height="{fit.h}%">
      {#if scrubbing && strip}
        <div
          class="strip"
          style:background-image="url('{strip}')"
          style:background-size="100% {tiles * 100}%"
          style:background-position-y={stripPosition(tile, tiles)}
        ></div>
      {:else if poster}
        <img src={poster} alt="" loading="lazy" draggable="false" />
      {:else}
        <div class="empty" class:failed={!!clip.error}>{clip.error ? 'No preview' : ''}</div>
      {/if}
    </div>
    {#if scrubbing && duration}
      <div class="playhead" style:left="{x}px"></div>
      <span class="badge mono">{formatTimecode(tileTime(tile, tiles, duration))}</span>
    {:else if duration !== null}
      <span class="badge mono">{formatTimecode(duration)}</span>
    {/if}
    {#if progress !== null}
      <div class="job" role="progressbar" aria-valuenow={progress} aria-valuemin={0} aria-valuemax={100} title="Making a Discord copy">
        <div style:width="{progress}%"></div>
      </div>
    {/if}
    {#if (clip.info?.bookmarks_ms.length ?? 0) > 0}
      <span class="marks" title="{clip.info?.bookmarks_ms.length} bookmark(s)">◆ {clip.info?.bookmarks_ms.length}</span>
    {/if}
    <button
      class="flat star"
      class:on={clip.starred}
      aria-label={clip.starred ? 'Unstar' : 'Star'}
      aria-pressed={clip.starred}
      title={clip.starred ? 'Starred: never removed to stay under the storage cap' : 'Star (keeps it from the storage cap)'}
      onclick={(e) => {
        e.stopPropagation();
        onstar();
      }}
      ondblclick={(e) => e.stopPropagation()}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true"
        ><path d="M8 1.6l1.9 4 4.4.5-3.3 3 .9 4.3L8 11.2l-3.9 2.2.9-4.3-3.3-3 4.4-.5z" /></svg
      >
    </button>
  </div>
  <div class="meta">
    <span class="name">{title}</span>
    <span class="sub">
      <span class="game">{clip.game ?? 'No game'}</span>
      <span class="dot" aria-hidden="true">·</span>
      <span class="mono">{detail}</span>
    </span>
  </div>
</div>

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 6px;
    border-radius: var(--radius);
    border: 1.5px solid transparent;
    cursor: default;
    min-width: 0;
    outline: none;
  }
  .card:hover {
    background: var(--hover);
  }
  .card.selected {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    border-color: var(--accent);
  }
  .card.focused:not(.selected) {
    border-color: var(--border-strong);
  }
  .thumb {
    position: relative;
    aspect-ratio: 16 / 9;
    background: var(--viewer);
    border-radius: var(--radius-sm);
    overflow: hidden;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .frame {
    position: relative;
  }
  img,
  .strip,
  .empty {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
  img {
    object-fit: cover;
    display: block;
  }
  .strip {
    background-repeat: no-repeat;
  }
  .empty {
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--text-muted);
    font-size: var(--text-xs);
    background: linear-gradient(110deg, var(--viewer) 30%, var(--panel-raised) 50%, var(--viewer) 70%);
    background-size: 300% 100%;
    animation: shimmer 1.6s linear infinite;
  }
  .empty.failed {
    animation: none;
    background: var(--viewer);
  }
  @keyframes shimmer {
    from {
      background-position: 100% 0;
    }
    to {
      background-position: 0 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .empty {
      animation: none;
    }
  }
  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    margin-left: -1px;
    background: var(--accent-text);
    pointer-events: none;
  }
  .badge {
    position: absolute;
    right: 6px;
    bottom: 6px;
    font-size: var(--text-xs);
    color: #f2f2f2;
    background: rgb(0 0 0 / 72%);
    padding: 1px 5px;
    border-radius: 3px;
    pointer-events: none;
  }
  .marks {
    position: absolute;
    left: 6px;
    bottom: 6px;
    font-size: var(--text-xs);
    color: #9fc6ff;
    background: rgb(0 0 0 / 72%);
    padding: 1px 5px;
    border-radius: 3px;
    pointer-events: none;
  }
  .star {
    position: absolute;
    top: 4px;
    right: 4px;
    width: 26px;
    height: 26px;
    padding: 0;
    justify-content: center;
    border-radius: 50%;
    background: rgb(0 0 0 / 55%);
    opacity: 0;
    transition: opacity var(--motion);
  }
  .star svg {
    width: 15px;
    height: 15px;
    fill: none;
    stroke: #f2f2f2;
    stroke-width: 1.3;
    stroke-linejoin: round;
  }
  .card:hover .star,
  .star.on,
  .star:focus-visible {
    opacity: 1;
  }
  .star.on svg {
    fill: #ffc940;
    stroke: #ffc940;
  }
  .star:hover:not(:disabled) {
    background: rgb(0 0 0 / 75%);
  }
  .meta {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
    padding: 0 2px;
  }
  .name {
    color: var(--text-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sub {
    display: flex;
    gap: 5px;
    font-size: var(--text-sm);
    color: var(--text-muted);
    min-width: 0;
  }
  .game {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sub .mono {
    flex: none;
  }
  .job {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    height: 4px;
    background: rgb(0 0 0 / 50%);
  }
  .job div {
    height: 100%;
    background: var(--accent);
  }
</style>
