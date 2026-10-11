<!--
  The Advanced workspace (ADR 0018, M9.5): media and effects on one side, the canvas
  preview with its transport in the middle, the inspector / history / export on the
  other side, the multi-track timeline below. The sides can swap and the panels resize
  (kept per viewer). Keys: Space play, J/K/L shuttle, S split, Ctrl+K split at the
  playhead (the selection, else every track), V / C the Selection / Razor tool, Ctrl+A
  select all, Delete remove (Shift: ripple delete), M marker, [ ] trim the selected clip
  to the playhead, ←/→ a frame (Shift: a second), Home/End, Ctrl+Z / Ctrl+Y.
-->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  import type { Source } from '../../lib/advanced';
  import {
    addMarker,
    clipLength,
    cutsAt,
    findClip,
    laneValue,
    overlapOf,
    readAdvanced,
    removeClips,
    rippleDelete,
    setLane,
    setTransitionAt,
    splitClip,
    splitClipsAt,
    trimClipEnd,
    trimClipStart,
  } from '../../lib/advanced';
  import { call, thumbUrl } from '../../lib/api';
  import { compose, layerActive } from '../../lib/compose';
  import type { Layer } from '../../lib/compose';
  import {
    beginGesture,
    closeEditor,
    cutSelection,
    MAIN_LOCKED,
    ed,
    edit,
    endGesture,
    gesture,
    liveEdit,
    placedInfos,
    redo,
    seek,
    selectClips,
    selectedClips,
    splitAtPlayhead,
    undo,
  } from '../../lib/editor.svelte';
  import { formatTimecode } from '../../lib/format';
  import { lib } from '../../lib/library.svelte';
  import Panel from '../../lib/Panel.svelte';
  import type { Project } from '../../lib/timeline';
  import { cut, segments, split as splitMain } from '../../lib/timeline';
  import NormalTools from '../NormalTools.svelte';
  import Canvas from './Canvas.svelte';
  import EffectsPanel from './EffectsPanel.svelte';
  import HistoryPanel from './HistoryPanel.svelte';
  import EffectControls from './EffectControls.svelte';
  import Meters from './Meters.svelte';
  import ProjectPanel from './ProjectPanel.svelte';
  import Properties from './Properties.svelte';
  import type { Player, SourceMedia } from './player';
  import Timeline from './Timeline.svelte';

  let { project, onmode, exportFacts }: { project: Project; onmode: (m: 'normal' | 'advanced') => void; exportFacts: import('svelte').Snippet } = $props();

  const clip = $derived(ed.clip!);
  const source = $derived(ed.source!);
  const comp = $derived(compose(project, source, placedInfos()));
  const names = $derived(new Map(ed.clips.map((c) => [c.key, c.name])));
  const frameMs = $derived(source.fps ? Math.max(1, Math.round(1000 / source.fps)) : 17);
  let player = $state<Player | null>(null);

  // ---- layout (FilmCraft's Editing workspace), kept per viewer ----
  interface Layout {
    topLeft: number;
    topRight: number;
    bottomLeft: number;
    bottom: number;
  }
  type Split = keyof Layout;
  const LAYOUT_KEY = 'klip.editor.layout.v2';
  const LIMITS: Record<Split, [number, number]> = { topLeft: [240, 680], topRight: [220, 560], bottomLeft: [200, 560], bottom: [160, 720] };
  function loadLayout(): Layout {
    const d: Layout = { topLeft: 440, topRight: 290, bottomLeft: 300, bottom: 300 };
    try {
      const raw = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? 'null') as Partial<Layout> | null;
      // The timeline height carries over from the old layout.
      const old = JSON.parse(localStorage.getItem('klip.editor.layout') ?? 'null') as { timeline?: unknown } | null;
      if (old && typeof old.timeline === 'number' && Number.isFinite(old.timeline)) d.bottom = old.timeline + 40;
      if (raw && typeof raw === 'object')
        for (const k of Object.keys(LIMITS) as Split[]) if (typeof raw[k] === 'number' && Number.isFinite(raw[k])) d[k] = raw[k];
      for (const k of Object.keys(LIMITS) as Split[]) d[k] = Math.min(LIMITS[k][1], Math.max(LIMITS[k][0], d[k]));
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
  let split: { which: Split; start: number; from: number } | null = null;
  function splitDown(e: PointerEvent, which: Split) {
    split = { which, start: which === 'bottom' ? e.clientY : e.clientX, from: layout[which] };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function splitMove(e: PointerEvent) {
    const s = split;
    if (!s) return;
    const d = (s.which === 'bottom' ? e.clientY : e.clientX) - s.start;
    const v = s.which === 'bottom' || s.which === 'topRight' ? s.from - d : s.from + d;
    layout[s.which] = Math.min(LIMITS[s.which][1], Math.max(LIMITS[s.which][0], v));
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

  /** A source's sound files for the timeline's waveform: all its tracks, else the file. */
  function soundOf(s: Source): string[] {
    const m = resolve(s);
    if (!m) return [];
    const tracks = m.tracks.map((t) => t.url).filter((u): u is string => !!u);
    return tracks.length ? tracks : m.url ? [m.url] : [];
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
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 'k') {
      e.preventDefault();
      splitAtPlayheadAll();
      return;
    }
    if ((e.ctrlKey || e.metaKey) && !e.altKey && e.key.toLowerCase() === 'a') {
      e.preventDefault();
      selectClips(readAdvanced(project).tracks.flatMap((t) => t.clips.map((c) => c.id)), ed.clipId);
      return;
    }
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const f = found;
    switch (e.key) {
      case 'v':
      case 'V':
        ed.tool = 'select';
        break;
      case 'c':
      case 'C':
        ed.tool = 'razor';
        break;
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
        else if (ed.mainLocked) ed.error = MAIN_LOCKED;
        else splitAtPlayhead();
        break;
      case 'Delete':
      case 'Backspace':
        if (ed.transitionSel) {
          const { track, after } = ed.transitionSel;
          edit((p) => setTransitionAt(p, track, after, 'cut', 0), 'Remove a transition');
          ed.transitionSel = null;
        } else if (ed.mainSel !== null && !ed.mainLocked) {
          // A piece of unlocked Main: what follows closes up (Main plays back to back).
          const s = segments(project)[ed.mainSel];
          if (s) edit((p) => cut(p, s.out_start_ms, s.out_end_ms), 'Remove a piece of Main');
          ed.mainSel = null;
        } else if (f) {
          // Shift: ripple delete (the later clips of each track close the gap).
          const ids = selectedClips();
          const many = ids.length > 1;
          if (e.shiftKey) edit((p) => rippleDelete(p, ids), many ? 'Ripple delete clips' : 'Ripple delete');
          else edit((p) => removeClips(p, ids), many ? 'Remove clips' : 'Remove clip');
          selectClips([]);
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
        else if (ed.tool === 'razor') ed.tool = 'select';
        else selectClips([]);
        break;
    }
  }

  /**
   * Ctrl+K (FilmCraft's Add Edit): cut the selected clips at the playhead, or, when none
   * of them plays there, every track (Main too, when unlocked) — one undo step.
   */
  function splitAtPlayheadAll() {
    const t = ed.playhead;
    const adv = readAdvanced(project);
    const ids = selectedClips();
    if (ids.length && cutsAt(adv, ids, t).length) {
      edit((p) => void splitClipsAt(p, ids, t), 'Split at the playhead');
      return;
    }
    const clips = cutsAt(adv, null, t).length;
    const inMain = segments(project).some((s) => t > s.out_start_ms && t < s.out_end_ms);
    const main = inMain && !ed.mainLocked;
    if (clips === 0 && !main) {
      if (inMain) ed.error = MAIN_LOCKED;
      return;
    }
    edit((p) => {
      splitClipsAt(p, null, t);
      if (main) splitMain(p, t);
    }, 'Split at the playhead');
  }
</script>

<svelte:window onkeydown={keydown} />

{#snippet splitter(which: Split, dir: 'v' | 'h')}
  <div
    class="splitter {dir}"
    role="separator"
    aria-orientation={dir === 'v' ? 'vertical' : 'horizontal'}
    onpointerdown={(e) => splitDown(e, which)}
    onpointermove={splitMove}
    onpointerup={splitUp}
    onpointercancel={splitUp}
  ></div>
{/snippet}

<div class="ws" style:grid-template-rows="minmax(0, 1fr) 4px {layout.bottom}px">
  <div class="row3" style:grid-template-columns="{layout.topLeft}px 4px minmax(0, 1fr) 4px {layout.topRight}px">
    <div class="cell">
      <Panel tabs={['Effect Controls', 'Effects', 'History']}>
        {#snippet children(tab)}
          {#if tab === 'Effect Controls'}
            <EffectControls {project} mainName={clip.name} {names} />
          {:else if tab === 'Effects'}
            <EffectsPanel {project} {comp} part="effects" />
          {:else}
            <HistoryPanel />
          {/if}
        {/snippet}
      </Panel>
    </div>
    {@render splitter('topLeft', 'v')}
    <div class="cell">
      <Panel tabs={['Program']} flush>
        {#snippet actions()}
          <span class="clipname" title={clip.name}>{clip.name}</span>
          <div class="mode" role="group" aria-label="Mode">
            <button class="flat" onclick={() => onmode('normal')}>Normal</button>
            <button class="flat on" aria-pressed="true">Advanced</button>
          </div>
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
            <div class="monitor-bar">
              <span class="tc mono" title="Playhead">{formatTimecode(ed.playhead)}</span>
              {#if shuttle !== 0 && shuttle !== 1}<span class="shuttle mono">{shuttle}x</span>{/if}
              <span class="grow"></span>
              {#if ed.media.status === 'making'}<span class="muted">Making a preview…</span>{/if}
              {#if ed.saving}<span class="muted">Saving…</span>{:else if ed.dirty}<span class="muted">Edited</span>{/if}
              <span class="dur mono" title="Length of the edit">{formatTimecode(comp.duration_ms)}</span>
            </div>
            <div class="transport">
              <span class="side"></span>
              <div class="buttons">
                <button class="icon" title="Go to the start" onclick={() => seek(0)}>⏮</button>
                <button class="icon" title="Back one frame (←)" onclick={() => seek(ed.playhead - frameMs)}>◀|</button>
                <button class="icon" title="Play backwards (J; again: faster)" onclick={() => setShuttle(shuttle >= 0 ? -1 : Math.max(-4, shuttle * 2))}>◀◀</button>
                <button class="icon play" onclick={togglePlay} title="Play / pause (Space)" aria-label={ed.playing ? 'Pause' : 'Play'}>{ed.playing ? '❚❚' : '▶'}</button>
                <button class="icon" title="Play faster (L; again: faster)" onclick={() => setShuttle(shuttle <= 0 ? 1 : Math.min(4, shuttle * 2))}>▶▶</button>
                <button class="icon" title="Forward one frame (→)" onclick={() => seek(ed.playhead + frameMs)}>|▶</button>
                <button class="icon" title="Go to the end" onclick={() => seek(Math.max(0, comp.duration_ms - 1))}>⏭</button>
              </div>
              <span class="side end">
                <button class="icon" disabled={ed.undo.length === 0} onclick={() => undo()} title="Undo {ed.undoLabels.at(-1) ?? ''} (Ctrl+Z)">↶</button>
                <button class="icon" disabled={ed.redo.length === 0} onclick={() => redo()} title="Redo (Ctrl+Y)">↷</button>
              </span>
            </div>
            {#if ed.error}<p class="error" role="alert">{ed.error}</p>{/if}
          </div>
      </Panel>
    </div>
    {@render splitter('topRight', 'v')}
    <div class="cell">
      <Panel tabs={['Properties', 'Main clip', 'Export']}>
        {#snippet children(tab)}
          {#if tab === 'Properties'}
            <Properties {project} mainName={clip.name} {names} />
          {:else if tab === 'Main clip'}
            <NormalTools {project} advanced />
          {:else}
            {@render exportFacts()}
          {/if}
        {/snippet}
      </Panel>
    </div>
  </div>
  {@render splitter('bottom', 'h')}
  <div class="row3" style:grid-template-columns="{layout.bottomLeft}px 4px minmax(0, 1fr) 4px 78px">
    <div class="cell">
      <Panel tabs={['Project', 'Markers']} flush>
        {#snippet children(tab)}
          {#if tab === 'Project'}
            <ProjectPanel />
          {:else}
            <div class="pad"><EffectsPanel {project} {comp} part="markers" /></div>
          {/if}
        {/snippet}
      </Panel>
    </div>
    {@render splitter('bottomLeft', 'v')}
    <div class="cell">
      <Panel tabs={['Timeline']} flush>
          <Timeline {comp} {project} mainName={clip.name} {names} {sourceMs} thumbs={(s) => resolve(s)?.strip ?? null} audio={soundOf} />
      </Panel>
    </div>
    <div class="gap"></div>
    <div class="cell meters-cell">
      <Meters {player} />
    </div>
  </div>
</div>

<style>
  .ws {
    display: grid;
    height: 100%;
    min-height: 0;
    background: var(--gutter);
  }
  .row3 {
    display: grid;
    min-height: 0;
    min-width: 0;
  }
  .cell {
    display: flex;
    min-height: 0;
    min-width: 0;
  }
  .cell > :global(*) {
    flex: 1;
  }
  .gap {
    background: var(--gutter);
  }
  .splitter {
    background: var(--gutter);
    touch-action: none;
  }
  .splitter.v {
    cursor: col-resize;
  }
  .splitter.h {
    cursor: row-resize;
  }
  .splitter:hover {
    background: var(--accent);
  }
  .stage {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    background: var(--panel);
  }
  .stage > :global(:first-child) {
    flex: 1;
    min-height: 0;
  }
  .monitor-bar {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 4px 10px 0;
    font-size: var(--text-sm);
  }
  .tc {
    color: var(--accent-text);
    font-size: 15px;
  }
  .dur {
    color: var(--text);
    font-size: 15px;
  }
  .transport {
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    padding: 2px 10px 6px;
  }
  .transport .buttons {
    display: flex;
    gap: 2px;
  }
  .transport .side.end {
    display: flex;
    justify-content: flex-end;
    gap: 2px;
  }
  .icon {
    min-width: 30px;
    height: 26px;
    padding: 0 6px;
    background: transparent;
    border: none;
    color: var(--text);
    font-size: 12px;
  }
  .icon:hover:not(:disabled) {
    color: var(--text-strong);
    background: var(--hover);
  }
  .icon.play {
    font-size: 15px;
    color: var(--text-strong);
  }
  .clipname {
    align-self: center;
    max-width: 220px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text-muted);
    font-size: var(--text-xs);
  }
  .pad {
    padding: var(--space-2) var(--space-3);
    overflow: auto;
  }
  .meters-cell {
    background: var(--panel);
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
