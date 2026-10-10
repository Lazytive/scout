<!--
  The Advanced workspace (ADR 0018, M9.5): media and effects on one side, the canvas
  preview with its transport in the middle, the inspector / history / export on the
  other side, the multi-track timeline below. The sides can swap and the panels resize
  (kept per viewer). Keys: Space play, J/K/L shuttle, S split, Delete remove, M marker,
  [ ] trim the selected clip to the playhead, ←/→ a frame (Shift: a second), Home/End,
  Ctrl+Z / Ctrl+Y.
-->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  import type { Source } from '../../lib/advanced';
  import { addMarker, clipLength, findClip, laneValue, overlapOf, readAdvanced, removeClip, setLane, splitClip, trimClipEnd, trimClipStart } from '../../lib/advanced';
  import { call, thumbUrl } from '../../lib/api';
  import { compose, layerActive } from '../../lib/compose';
  import type { Layer } from '../../lib/compose';
  import {
    beginGesture,
    closeEditor,
    cutSelection,
    ed,
    edit,
    endGesture,
    gesture,
    liveEdit,
    placedInfos,
    redo,
    seek,
    splitAtPlayhead,
    undo,
  } from '../../lib/editor.svelte';
  import { formatTimecode } from '../../lib/format';
  import { lib } from '../../lib/library.svelte';
  import Panel from '../../lib/Panel.svelte';
  import type { Project } from '../../lib/timeline';
  import NormalTools from '../NormalTools.svelte';
  import Canvas from './Canvas.svelte';
  import EffectsPanel from './EffectsPanel.svelte';
  import HistoryPanel from './HistoryPanel.svelte';
  import Inspector from './Inspector.svelte';
  import MediaPanel from './MediaPanel.svelte';
  import type { Player, SourceMedia } from './player';
  import Timeline from './Timeline.svelte';

  let { project, onmode, exportFacts }: { project: Project; onmode: (m: 'normal' | 'advanced') => void; exportFacts: import('svelte').Snippet } = $props();

  const clip = $derived(ed.clip!);
  const source = $derived(ed.source!);
  const comp = $derived(compose(project, source, placedInfos()));
  const names = $derived(new Map(ed.clips.map((c) => [c.key, c.name])));
  const frameMs = $derived(source.fps ? Math.max(1, Math.round(1000 / source.fps)) : 17);
  let player = $state<Player | null>(null);

  // ---- layout, kept per viewer ----
  interface Layout {
    left: number;
    right: number;
    timeline: number;
    swapped: boolean;
  }
  const LAYOUT_KEY = 'klip.editor.layout';
  function loadLayout(): Layout {
    const d: Layout = { left: 260, right: 290, timeline: 260, swapped: false };
    try {
      const raw = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? 'null') as Partial<Layout> | null;
      if (raw && typeof raw === 'object') {
        for (const k of ['left', 'right', 'timeline'] as const) if (typeof raw[k] === 'number' && Number.isFinite(raw[k])) d[k] = raw[k];
        d.swapped = raw.swapped === true;
      }
    } catch {
      // Defaults.
    }
    return d;
  }
  let layout = $state(loadLayout());
  $effect(() => {
    const snapshot = JSON.stringify(layout);
    try {
      localStorage.setItem(LAYOUT_KEY, snapshot);
    } catch {
      // Not kept; fine.
    }
  });
  let split: { which: 'left' | 'right' | 'timeline'; start: number; from: number } | null = null;
  function splitDown(e: PointerEvent, which: 'left' | 'right' | 'timeline') {
    split = { which, start: which === 'timeline' ? e.clientY : e.clientX, from: layout[which] };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function splitMove(e: PointerEvent) {
    const s = split;
    if (!s) return;
    if (s.which === 'timeline') layout.timeline = Math.min(640, Math.max(140, s.from - (e.clientY - s.start)));
    else {
      const d = e.clientX - s.start;
      const leftSide = (s.which === 'left') !== layout.swapped;
      layout[s.which] = Math.min(520, Math.max(200, s.from + (leftSide ? d : -d)));
    }
  }
  function splitUp() {
    split = null;
  }

  // ---- media ----
  function stripOf(key: string, durationMs: number) {
    const c = lib.listing?.clips.find((x) => x.key === key);
    if (!c?.thumbs.strip || !lib.listing) return null;
    return { url: thumbUrl('strip', key, c.thumbs.strip), tiles: lib.listing.strip.tiles, duration_ms: durationMs };
  }

  function resolve(s: Source): SourceMedia | null {
    const src = ed.source;
    if (!src) return null;
    switch (s.kind) {
      case 'main':
        return {
          url: ed.media.url,
          tracks: ed.tracks.flatMap((t) => (t.url ? [{ name: t.name, url: t.url }] : [])),
          image: null,
          width: src.width,
          height: src.height,
          strip: ed.clip ? stripOf(ed.clip.key, src.duration_ms) : null,
        };
      case 'library': {
        const c = ed.clips.find((x) => x.key === s.key);
        if (!c?.source) return null;
        const e = ed.sources[`lib:${s.key}`];
        return { url: e?.url ?? null, tracks: e?.tracks ?? [], image: null, width: c.source.width, height: c.source.height, strip: stripOf(s.key, c.source.duration_ms) };
      }
      case 'file':
        return { url: ed.sources[`file:${s.path}`]?.url ?? null, tracks: [], image: null, width: 0, height: 0, strip: null };
      case 'image':
        return { url: null, tracks: [], image: ed.sources[`image:${s.path}`]?.image ?? null, width: 0, height: 0, strip: null };
    }
  }

  /** A source's length (ms) to bound a trim; 0 when unknown (no bound). */
  function sourceMs(c: { source: Source; out_ms: number }): number {
    if (c.source.kind === 'main') return source.duration_ms;
    if (c.source.kind === 'library') {
      const key = c.source.key;
      return ed.clips.find((x) => x.key === key)?.source?.duration_ms ?? 0;
    }
    return 0;
  }

  // ---- the selected clip on the canvas ----
  const found = $derived(ed.clipId === null ? null : findClip(readAdvanced(project), ed.clipId));
  /** How far the selected clip plays before its `at_ms` (a crossfade into it). */
  const shift = $derived(found ? overlapOf(readAdvanced(project), found.track, found.clip) : 0);
  /** The playhead in the selected clip's own time. */
  function localOf(f: NonNullable<typeof found>): number {
    return Math.min(clipLength(f.clip), Math.max(0, ed.playhead - (f.clip.at_ms - shift)));
  }
  const selectedLayer = $derived.by((): Layer | null => {
    const f = found;
    if (!f) return null;
    return (
      comp.layers.find(
        (l) => l.track === f.track.id && l.src_in_ms === f.clip.in_ms && l.src_out_ms === f.clip.out_ms && layerActive(l, ed.playhead) && (l.kind === 'video' || l.kind === 'image'),
      ) ?? null
    );
  });
  let moveBase: { x: number; y: number } | null = null;
  function onmove(dx: number, dy: number, done: boolean) {
    const f = found;
    if (!f) return;
    const local = localOf(f);
    if (!moveBase) {
      moveBase = { x: laneValue(f.clip, 'x', local), y: laneValue(f.clip, 'y', local) };
      beginGesture();
    }
    const base = moveBase;
    gesture((p) => {
      setLane(p, f.clip.id, 'x', local, base.x + dx);
      setLane(p, f.clip.id, 'y', local, base.y + dy);
    });
    if (done) {
      moveBase = null;
      endGesture('Move picture');
    }
  }
  function onzoom(factor: number) {
    const f = found;
    if (!f) return;
    const local = localOf(f);
    const z = laneValue(f.clip, 'zoom', local) * factor;
    liveEdit((p) => setLane(p, f.clip.id, 'zoom', local, z), 'Zoom');
  }

  // ---- the parity check (M9.5 acceptance): the canvas vs the export's own frame ----
  interface Parity {
    psnr: number;
    width: number;
    height: number;
    canvas?: string;
    engine?: string;
  }

  /** PSNR (dB) of two RGBA buffers over R, G and B. */
  function psnr(a: Uint8ClampedArray, b: Uint8ClampedArray): number {
    let sum = 0;
    let n = 0;
    for (let i = 0; i < Math.min(a.length, b.length); i += 4) {
      for (let c = 0; c < 3; c++) {
        const d = a[i + c]! - b[i + c]!;
        sum += d * d;
      }
      n += 3;
    }
    if (n === 0) return 0;
    const mse = sum / n;
    return mse === 0 ? Infinity : 10 * Math.log10((255 * 255) / mse);
  }

  async function parity(outMs: number, width: number, images = false): Promise<Parity> {
    const p = player;
    const c = ed.clip;
    const canvas = document.querySelector<HTMLCanvasElement>('canvas[aria-label="Preview"]');
    if (!p || !c || !canvas) throw new Error('no preview');
    ed.playing = false;
    const r = await call<{ jpeg: string; width: number; height: number }>('editor.frame', { path: c.path, out_ms: outMs, width, project: ed.project });
    const img = new Image();
    img.src = `data:image/jpeg;base64,${r.jpeg}`;
    await img.decode();
    const ref = document.createElement('canvas');
    ref.width = r.width;
    ref.height = r.height;
    const rc = ref.getContext('2d');
    if (!rc) throw new Error('no 2d context');
    rc.drawImage(img, 0, 0);
    const before = { w: canvas.width, h: canvas.height };
    p.resize(r.width, r.height);
    await p.frameAt(outMs);
    const mine = canvas.getContext('2d')?.getImageData(0, 0, r.width, r.height).data;
    const theirs = rc.getImageData(0, 0, r.width, r.height).data;
    const out: Parity = { psnr: mine ? psnr(mine, theirs) : 0, width: r.width, height: r.height };
    if (images) {
      out.canvas = canvas.toDataURL('image/png');
      out.engine = img.src;
    }
    p.resize(before.w, before.h);
    return out;
  }

  onMount(() => {
    const w = window as unknown as { __klipTest?: { parity: typeof parity } };
    w.__klipTest = { parity };
    return () => delete w.__klipTest;
  });

  // ---- transport: J/K/L ----
  let shuttle = $state(0);
  let reverse: ReturnType<typeof setInterval> | null = null;
  function stopReverse() {
    if (reverse) clearInterval(reverse);
    reverse = null;
  }
  function setShuttle(v: number) {
    shuttle = v;
    stopReverse();
    if (v > 0) {
      player?.setRate(v);
      ed.playing = true;
    } else if (v < 0) {
      ed.playing = false;
      const step = 40;
      reverse = setInterval(() => {
        const t = ed.playhead - step * -v;
        seek(t);
        if (t <= 0) setShuttle(0);
      }, step);
    } else {
      ed.playing = false;
      player?.setRate(1);
    }
  }
  function togglePlay() {
    stopReverse();
    shuttle = 0;
    player?.setRate(1);
    ed.playing = !ed.playing;
  }
  onDestroy(stopReverse);
  $effect(() => {
    if (!ed.playing && shuttle > 0) shuttle = 0;
  });

  function keydown(e: KeyboardEvent) {
    const target = e.target as HTMLElement | null;
    if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT')) return;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z') {
      e.preventDefault();
      if (e.shiftKey) redo();
      else undo();
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'y') {
      e.preventDefault();
      redo();
      return;
    }
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const f = found;
    switch (e.key) {
      case ' ':
        e.preventDefault();
        togglePlay();
        break;
      case 'l':
      case 'L':
        setShuttle(shuttle <= 0 ? 1 : Math.min(4, shuttle * 2));
        break;
      case 'k':
      case 'K':
        setShuttle(0);
        break;
      case 'j':
      case 'J':
        setShuttle(shuttle >= 0 ? -1 : Math.max(-4, shuttle * 2));
        break;
      case 's':
      case 'S':
        if (f && localOf(f) > 0 && localOf(f) < clipLength(f.clip)) edit((p) => void splitClip(p, f.clip.id, f.clip.at_ms + localOf(f)), 'Split clip');
        else splitAtPlayhead();
        break;
      case 'Delete':
      case 'Backspace':
        if (f) {
          edit((p) => removeClip(p, f.clip.id), 'Remove clip');
          ed.clipId = null;
        } else cutSelection();
        break;
      case 'm':
      case 'M':
        edit((p) => addMarker(p, ed.playhead, ''), 'Add a marker');
        break;
      case '[':
        if (f) edit((p) => trimClipStart(p, f.clip.id, ed.playhead + shift), 'Trim clip');
        break;
      case ']':
        if (f) edit((p) => trimClipEnd(p, f.clip.id, ed.playhead + shift, sourceMs(f.clip)), 'Trim clip');
        break;
      case 'ArrowLeft':
        e.preventDefault();
        setShuttle(0);
        seek(ed.playhead - (e.shiftKey ? 1000 : frameMs));
        break;
      case 'ArrowRight':
        e.preventDefault();
        setShuttle(0);
        seek(ed.playhead + (e.shiftKey ? 1000 : frameMs));
        break;
      case 'Home':
        seek(0);
        break;
      case 'End':
        seek(comp.duration_ms - 1);
        break;
      case 'Escape':
        if (ed.exportOpen) ed.exportOpen = false;
        else ed.clipId = null;
        break;
    }
  }
</script>

<svelte:window onkeydown={keydown} />

{#snippet sideA()}
  <Panel tabs={['Media', 'Effects', 'Main clip']}>
    {#snippet children(tab)}
      {#if tab === 'Media'}
        <MediaPanel />
      {:else if tab === 'Effects'}
        <EffectsPanel {project} {comp} />
      {:else}
        <NormalTools {project} advanced />
      {/if}
    {/snippet}
  </Panel>
{/snippet}

{#snippet sideB()}
  <Panel tabs={['Inspector', 'History', 'Export']}>
    {#snippet children(tab)}
      {#if tab === 'Inspector'}
        <Inspector {project} mainName={clip.name} {names} />
      {:else if tab === 'History'}
        <HistoryPanel />
      {:else}
        {@render exportFacts()}
      {/if}
    {/snippet}
  </Panel>
{/snippet}

<div
  class="ws"
  style:grid-template-columns="{layout.swapped ? layout.right : layout.left}px 4px minmax(0, 1fr) 4px {layout.swapped ? layout.left : layout.right}px"
  style:grid-template-rows="minmax(0, 1fr) 4px {layout.timeline}px"
>
  <div class="side">{@render (layout.swapped ? sideB : sideA)()}</div>
  <div
    class="splitter v"
    role="separator"
    aria-orientation="vertical"
    onpointerdown={(e) => splitDown(e, layout.swapped ? 'right' : 'left')}
    onpointermove={splitMove}
    onpointerup={splitUp}
    onpointercancel={splitUp}
  ></div>
  <div class="center">
    <Panel tabs={[clip.name]}>
      {#snippet actions()}
        <div class="mode" role="group" aria-label="Mode">
          <button class="flat" onclick={() => onmode('normal')}>Normal</button>
          <button class="flat on" aria-pressed="true">Advanced</button>
        </div>
        <button class="flat" title="Swap the side panels" onclick={() => (layout.swapped = !layout.swapped)}>⇄</button>
        <button class="flat" onclick={closeEditor}>← Library</button>
      {/snippet}
        <div class="stage">
          <Canvas
            {comp}
            {resolve}
            playhead={ed.playhead}
            playing={ed.playing}
            cues={ed.cues}
            selected={selectedLayer}
            mediaVersion={ed.mediaVersion}
            onplayhead={(ms) => (ed.playhead = ms)}
            onended={() => {
              ed.playing = false;
              shuttle = 0;
            }}
            {onmove}
            {onzoom}
            bind:player
          />
          <div class="transport">
            <button class="flat" title="Play backwards (J; again: faster)" onclick={() => setShuttle(shuttle >= 0 ? -1 : Math.max(-4, shuttle * 2))}>◀◀</button>
            <button class="primary" onclick={togglePlay} title="Play / pause (Space)">{ed.playing ? 'Pause' : 'Play'}</button>
            <button class="flat" title="Play faster (L again)" onclick={() => setShuttle(shuttle <= 0 ? 1 : Math.min(4, shuttle * 2))}>▶▶</button>
            {#if shuttle !== 0 && shuttle !== 1}<span class="shuttle mono">{shuttle}x</span>{/if}
            <span class="timecode mono">{formatTimecode(ed.playhead)}</span>
            <span class="muted mono">/ {formatTimecode(comp.duration_ms)}</span>
            <span class="grow"></span>
            {#if ed.media.status === 'making'}<span class="muted">Making a preview…</span>{/if}
            {#if ed.saving}<span class="muted">Saving…</span>{:else if ed.dirty}<span class="muted">Edited</span>{/if}
            <button class="flat" disabled={ed.undo.length === 0} onclick={() => undo()} title="Undo {ed.undoLabels.at(-1) ?? ''} (Ctrl+Z)">Undo</button>
            <button class="flat" disabled={ed.redo.length === 0} onclick={() => redo()} title="Redo (Ctrl+Y)">Redo</button>
          </div>
          {#if ed.error}<p class="error" role="alert">{ed.error}</p>{/if}
        </div>
    </Panel>
  </div>
  <div
    class="splitter v"
    role="separator"
    aria-orientation="vertical"
    onpointerdown={(e) => splitDown(e, layout.swapped ? 'left' : 'right')}
    onpointermove={splitMove}
    onpointerup={splitUp}
    onpointercancel={splitUp}
  ></div>
  <div class="side">{@render (layout.swapped ? sideA : sideB)()}</div>
  <div
    class="splitter h"
    role="separator"
    aria-orientation="horizontal"
    onpointerdown={(e) => splitDown(e, 'timeline')}
    onpointermove={splitMove}
    onpointerup={splitUp}
    onpointercancel={splitUp}
  ></div>
  <div class="bottom">
    <Timeline {comp} {project} mainName={clip.name} {names} {sourceMs} />
  </div>
</div>

<style>
  .ws {
    display: grid;
    height: 100%;
    min-height: 0;
  }
  .side,
  .center,
  .bottom {
    display: flex;
    min-height: 0;
    min-width: 0;
  }
  .side > :global(*),
  .center > :global(*),
  .bottom > :global(*) {
    flex: 1;
  }
  .bottom {
    grid-column: 1 / -1;
  }
  .splitter {
    background: var(--gutter);
    touch-action: none;
  }
  .splitter.v {
    cursor: col-resize;
  }
  .splitter.h {
    grid-column: 1 / -1;
    cursor: row-resize;
  }
  .splitter:hover {
    background: var(--accent);
  }
  .stage {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    height: 100%;
  }
  .transport {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .timecode {
    font-size: var(--text-timecode);
    color: var(--accent-text);
  }
  .shuttle {
    color: var(--warn);
  }
  .grow {
    flex: 1;
  }
  .mode {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    overflow: hidden;
  }
  .mode .on {
    background: var(--accent);
    color: var(--on-accent);
  }
  .mono {
    font-family: var(--font-mono);
  }
  .error {
    margin: 0;
    color: var(--bad);
    font-size: var(--text-sm);
  }
</style>
