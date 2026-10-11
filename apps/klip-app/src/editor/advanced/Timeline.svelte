<!--
  Advanced mode's timeline in FilmCraft's (Premiere's) look: a big blue timecode and the
  toggles top-left, the ruler with marker flags and the playhead handle, track headers
  (V2… the Advanced video tracks, V1 = Main, the Normal-mode cut; A1 = Main's sound, A2…
  sound and music), video above a divider and sound below, all in ONE scroller with a
  sticky header column and ruler, and a zoom bar at the bottom (drag the bar to scroll,
  its round ends to zoom; Ctrl/Alt+wheel zooms at the pointer).

  Clips drag along and across tracks of their kind and trim at their edges, but only
  once the pointer moved a few px (a click just selects). Everything snaps to the
  playhead, clip edges and markers (hold Alt to not snap). Transitions sit on the join
  as hatched blocks: click selects, drag an end to change the length, right-click for
  the menu; a + on a join without one adds a crossfade. Keyframes live in Effect
  Controls, not here.
-->
<script lang="ts">
  import { tick } from 'svelte';
  import type { PlacedClip, Source, Track, TransitionKind } from '../../lib/advanced';
  import {
    addMarker,
    clipEnd,
    clipLength,
    fitsTrack,
    moveClip,
    overlapOf,
    readAdvanced,
    FROM_BELOW,
    fromBelow,
    setFromBelow,
    setTrack,
    setTransitionAt,
    sourceLabel,
    trimClipEnd,
    trimClipStart,
  } from '../../lib/advanced';
  import type { Composition } from '../../lib/compose';
  import { beginGesture, ed, edit, endGesture, gesture, seek } from '../../lib/editor.svelte';
  import { formatTimecode } from '../../lib/format';
  import type { Project } from '../../lib/timeline';
  import { segments } from '../../lib/timeline';
  import { stripPosition } from '../../lib/library';
  import Waveform from './Waveform.svelte';
  import type { TimelineRow, TransitionBox } from '../../lib/timeline-view';
  import {
    DEFAULT_TRANSITION_MS,
    clipZone,
    fitPps,
    fromHandles,
    joins,
    pastThreshold,
    rowsOf,
    spanMs,
    thumb,
    transitionBoxes,
    transitionLength,
    zoomAt,
  } from '../../lib/timeline-view';

  let {
    comp,
    project,
    mainName,
    names,
    sourceMs,
    thumbs,
    audio,
  }: {
    comp: Composition;
    project: Project;
    mainName: string;
    /** Library clip names by key. */
    names: Map<string, string>;
    /** A source's length in ms (to bound trims), or 0 when unknown. */
    sourceMs: (c: PlacedClip) => number;
    /** A source's sprite strip for thumbnails, if the library has one. */
    thumbs: (s: Source) => { url: string; tiles: number; duration_ms: number } | null;
    /** A source's sound file for its waveform, if any. */
    audio: (s: Source) => string | null;
  } = $props();

  const HEADER_W = 168;
  const TOP_H = 52;
  const DIVIDER_H = 5;
  const SNAP_PX = 9;
  const MAX_PPS = 600;
  const ROW_MIN = 24;
  const ROW_MAX = 160;
  const HEIGHTS_KEY = 'klip.timeline.rows';

  let scroller = $state<HTMLDivElement | null>(null);
  let bar = $state<HTMLDivElement | null>(null);
  /** The visible width of the lanes (without the header column), px. */
  let viewW = $state(800);
  let scrollX = $state(0);
  /** Pixels per second; 0 = fit the whole edit. */
  let zoom = $state(0);
  let snapping = $state(true);
  /** The zoom held while a drag runs (in Fit the edit's length would rescale it live). */
  let frozen = $state<number | null>(null);
  let heights = $state(loadHeights());

  const adv = $derived(readAdvanced(project));
  const duration = $derived(Math.max(comp.duration_ms, 1000));
  const span = $derived(spanMs(duration));
  const fit = $derived(fitPps(span, viewW));
  const pps = $derived(frozen ?? (zoom > 0 ? Math.min(MAX_PPS, Math.max(fit, zoom)) : fit));
  const contentW = $derived(Math.ceil((span / 1000) * pps));
  const px = (ms: number) => (ms / 1000) * pps;
  const msAt = (x: number) => Math.max(0, Math.round((x / pps) * 1000));

  const segs = $derived(segments(project));
  const rows = $derived(rowsOf(adv));
  const boxes = $derived(transitionBoxes(adv));
  const touching = $derived([
    ...joins(adv, Math.max(1, (2 / pps) * 1000)),
    // A video track's first clip can crossfade in from whatever is below it.
    ...adv.tracks
      .filter((t) => (t.kind === 'video' || t.kind === 'text') && t.clips[0] && t.clips[0].at_ms > 0 && !fromBelow(adv, t.id))
      .map((t) => ({ track: t.id, after: FROM_BELOW, next: t.clips[0]!.id, at: t.clips[0]!.at_ms })),
  ]);
  const lastVideo = $derived(rows.findIndex((r) => r.kind === 'main'));
  const videoCount = $derived(adv.tracks.filter((t) => t.kind === 'video' || t.kind === 'text').length);
  const soundCount = $derived(adv.tracks.length - videoCount);
  const bar01 = $derived(thumb(scrollX, viewW, contentW));

  function loadHeights(): { video: number; sound: number } {
    const d = { video: 44, sound: 40 };
    try {
      const raw = JSON.parse(localStorage.getItem(HEIGHTS_KEY) ?? 'null') as Partial<typeof d> | null;
      if (!raw || typeof raw !== 'object') return d;
      const ok = (v: unknown, fallback: number) => (typeof v === 'number' && Number.isFinite(v) ? clampH(v) : fallback);
      return { video: ok(raw.video, d.video), sound: ok(raw.sound, d.sound) };
    } catch {
      return d;
    }
  }
  $effect(() => {
    const snapshot = JSON.stringify(heights);
    try {
      localStorage.setItem(HEIGHTS_KEY, snapshot);
    } catch {
      // Storage off (private window): the heights just aren't kept.
    }
  });
  function clampH(h: number): number {
    return Math.round(Math.min(ROW_MAX, Math.max(ROW_MIN, h)));
  }
  const rowH = (r: TimelineRow) => (r.group === 'video' ? heights.video : heights.sound);

  function observe(el: HTMLDivElement) {
    const ro = new ResizeObserver(() => (viewW = Math.max(100, el.clientWidth - HEADER_W)));
    ro.observe(el);
    viewW = Math.max(100, el.clientWidth - HEADER_W);
    return { destroy: () => ro.disconnect() };
  }

  /** Ctrl/Alt + wheel zooms around the pointer (the plain wheel scrolls). */
  function wheelZoom(el: HTMLDivElement) {
    const on = (e: WheelEvent) => {
      if (!e.ctrlKey && !e.altKey) return;
      e.preventDefault();
      const r = el.getBoundingClientRect();
      void zoomTo(pps * (e.deltaY < 0 ? 1.25 : 0.8), e.clientX - r.left - HEADER_W);
    };
    el.addEventListener('wheel', on, { passive: false });
    return { destroy: () => el.removeEventListener('wheel', on) };
  }

  async function zoomTo(want: number, anchorX: number) {
    const el = scroller;
    const next = Math.min(MAX_PPS, Math.max(fit, want));
    const left = el ? zoomAt(pps, next, Math.max(0, anchorX), el.scrollLeft) : 0;
    zoom = next <= fit * 1.0001 ? 0 : next;
    await tick();
    if (el) el.scrollLeft = left;
  }

  // Ruler ticks: a step that keeps labels ~90 px apart; only the visible ones.
  const ticks = $derived.by(() => {
    const steps = [100, 250, 500, 1000, 2000, 5000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000];
    const step = steps.find((s) => px(s) >= 90) ?? 600_000;
    const from = Math.max(0, Math.floor(msAt(scrollX - 100) / step) * step);
    const to = Math.min(span, msAt(scrollX + viewW + 100));
    const out: number[] = [];
    for (let t = from; t <= to && out.length < 400; t += step) out.push(t);
    return { step, list: out };
  });

  /** A ruler label: whole seconds as a timecode, finer steps in seconds. */
  function tickLabel(t: number): string {
    return ticks.step >= 1000 ? formatTimecode(t) : `${(t / 1000).toFixed(ticks.step >= 500 ? 1 : 2)} s`;
  }

  function snapTargets(exceptClip: number | null): number[] {
    const out = [0, ed.playhead, comp.duration_ms];
    for (const s of segs) out.push(s.out_start_ms, s.out_end_ms);
    for (const t of adv.tracks) for (const c of t.clips) if (c.id !== exceptClip) out.push(c.at_ms, clipEnd(c));
    for (const m of comp.markers) out.push(m.at_ms);
    return out;
  }

  function snap(ms: number, targets: number[], alt: boolean): number {
    if (!snapping || alt) return ms;
    const reach = (SNAP_PX / pps) * 1000;
    let best = ms;
    let dist = reach;
    for (const t of targets) {
      const d = Math.abs(t - ms);
      if (d < dist) {
        dist = d;
        best = t;
      }
    }
    return best;
  }

  /** The clip's start moved to `at`, snapping either edge. */
  function snapMove(at: number, len: number, targets: number[], alt: boolean): number {
    const a = snap(at, targets, alt);
    if (a !== at) return a;
    const e = snap(at + len, targets, alt);
    return e !== at + len ? e - len : at;
  }

  // ---- ruler: scrub ----
  let scrubbing = false;
  function rulerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    scrubbing = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    rulerMove(e);
  }
  function rulerMove(e: PointerEvent) {
    if (!scrubbing) return;
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    ed.playing = false;
    seek(Math.min(duration, snap(msAt(e.clientX - r.left), snapTargets(null), e.altKey)));
  }
  function rulerUp() {
    scrubbing = false;
  }

  // ---- clips: select, then move or trim past the threshold ----
  type Press = {
    zone: 'move' | 'start' | 'end';
    clip: PlacedClip;
    track: Track;
    x: number;
    y: number;
    at: number;
    end: number;
    targets: number[];
    started: boolean;
  };
  let press = $state<Press | null>(null);
  let dropTrack = $state<number | 'new' | null>(null);

  /**
   * Follow a drag on the window: a clip that moves to another track is drawn by a new
   * element, which loses the pointer capture (and with it the release).
   */
  function follow(move: (e: PointerEvent) => void, up: () => void) {
    const onUp = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', onUp);
      window.removeEventListener('pointercancel', onUp);
      up();
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', onUp);
    window.addEventListener('pointercancel', onUp);
  }

  function select(clipId: number | null) {
    ed.clipId = clipId;
    ed.textId = null;
    ed.transitionSel = null;
  }

  function clipDown(e: PointerEvent, track: Track, clip: PlacedClip) {
    if (e.button !== 0) return;
    e.stopPropagation();
    select(clip.id);
    const el = e.currentTarget as HTMLElement;
    const r = el.getBoundingClientRect();
    const zone = clipZone(e.clientX - r.left, r.width);
    press = { zone, clip, track, x: e.clientX, y: e.clientY, at: clip.at_ms, end: clipEnd(clip), targets: snapTargets(clip.id), started: false };
    follow(clipMove, clipUp);
  }

  function clipHover(e: PointerEvent) {
    if (press) return;
    const el = e.currentTarget as HTMLElement;
    const r = el.getBoundingClientRect();
    el.style.cursor = clipZone(e.clientX - r.left, r.width) === 'move' ? '' : 'ew-resize';
  }

  function trackUnder(e: PointerEvent, clip: PlacedClip): number | 'new' | null {
    const el = document.elementFromPoint(e.clientX, e.clientY)?.closest('[data-track]') as HTMLElement | null;
    const raw = el?.dataset.track;
    if (!raw) return null;
    if (raw === 'new-video' || raw === 'new-sound') {
      const want = raw === 'new-video' ? 'video' : 'audio';
      return fitsTrack(clip.source, want) || (want === 'audio' && fitsTrack(clip.source, 'music')) ? 'new' : null;
    }
    const id = Number(raw);
    const t = adv.tracks.find((x) => x.id === id);
    return t && fitsTrack(clip.source, t.kind) ? id : null;
  }

  function clipMove(e: PointerEvent) {
    const d = press;
    if (!d) return;
    if (!d.started) {
      if (!pastThreshold(e.clientX - d.x, e.clientY - d.y)) return;
      d.started = true;
      frozen = pps;
      beginGesture();
    }
    const dms = ((e.clientX - d.x) / pps) * 1000;
    if (d.zone === 'move') {
      const len = d.end - d.at;
      const at = Math.max(0, snapMove(Math.round(d.at + dms), len, d.targets, e.altKey));
      const target = Math.abs(e.clientY - d.y) > rowH({ kind: 'main', label: '', group: 'video' }) / 2 ? trackUnder(e, d.clip) : null;
      dropTrack = target;
      gesture((p) => moveClip(p, d.clip.id, at, target === null ? undefined : target));
    } else if (d.zone === 'start') {
      const at = snap(Math.round(d.at + dms), d.targets, e.altKey);
      gesture((p) => trimClipStart(p, d.clip.id, at));
    } else {
      const end = snap(Math.round(d.end + dms), d.targets, e.altKey);
      gesture((p) => trimClipEnd(p, d.clip.id, end, sourceMs(d.clip)));
    }
  }

  function clipUp() {
    const d = press;
    press = null;
    dropTrack = null;
    frozen = null;
    if (!d || !d.started) return;
    endGesture(d.zone === 'move' ? 'Move clip' : 'Trim clip');
  }

  // ---- transitions ----
  const KIND_LABEL: Record<TransitionKind, string> = { cut: 'Cut', crossfade: 'Crossfade', dip: 'Dip to black' };
  type TPress = { box: TransitionBox; edge: 'start' | 'end'; x: number; y: number; started: boolean };
  let tpress: TPress | null = null;

  function selectTransition(b: TransitionBox) {
    ed.clipId = null;
    ed.textId = null;
    ed.transitionSel = { track: b.track, after: b.after };
  }
  const isSel = (b: TransitionBox) => ed.transitionSel?.track === b.track && ed.transitionSel.after === b.after;

  function transDown(e: PointerEvent, b: TransitionBox, edge: 'start' | 'end' | null) {
    if (e.button !== 0) return;
    e.stopPropagation();
    selectTransition(b);
    if (!edge) return;
    tpress = { box: b, edge, x: e.clientX, y: e.clientY, started: false };
    follow(transMove, transUp);
  }
  function transMove(e: PointerEvent) {
    const t = tpress;
    if (!t) return;
    if (!t.started) {
      if (!pastThreshold(e.clientX - t.x, e.clientY - t.y)) return;
      t.started = true;
      frozen = pps;
      beginGesture();
    }
    // The dragged end follows the pointer (a dip grows on both sides of the cut).
    const dms = ((e.clientX - t.x) / pps) * 1000;
    const ms = transitionLength(t.box.ms, t.edge, dms, t.box.max);
    gesture((p) => setTransitionAt(p, t.box.track, t.box.after, t.box.kind, ms));
  }
  function transUp() {
    const t = tpress;
    tpress = null;
    frozen = null;
    if (t?.started) endGesture('Transition length');
  }

  let menu = $state<{ x: number; y: number; box: TransitionBox } | null>(null);
  function transMenu(e: MouseEvent, b: TransitionBox) {
    e.preventDefault();
    e.stopPropagation();
    selectTransition(b);
    menu = { x: e.clientX, y: e.clientY, box: b };
  }
  function setKind(b: TransitionBox, kind: TransitionKind) {
    menu = null;
    edit((p) => setTransitionAt(p, b.track, b.after, kind, b.ms), kind === 'cut' ? 'Remove a transition' : KIND_LABEL[kind]);
    if (kind === 'cut') ed.transitionSel = null;
  }
  function addCrossfade(j: { track: number; after: number; next: number }) {
    const change = (p: Project) =>
      j.after === FROM_BELOW ? setFromBelow(p, j.next, DEFAULT_TRANSITION_MS) : setTransitionAt(p, j.track, j.after, 'crossfade', DEFAULT_TRANSITION_MS);
    if (edit(change, j.after === FROM_BELOW ? 'Crossfade in from below' : 'Add a crossfade')) {
      ed.clipId = null;
      ed.transitionSel = { track: j.track, after: j.after };
    }
  }

  // ---- empty space: deselect and put the playhead there ----
  function laneClick(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    select(null);
    ed.playing = false;
    seek(Math.min(duration, snap(msAt(e.clientX - r.left), snapTargets(null), e.altKey)));
  }

  // ---- track headers: mute, rename, height ----
  function mute(t: Track) {
    edit((p) => setTrack(p, t.id, { muted: !t.muted }), t.muted ? 'Unmute a track' : 'Mute a track');
  }
  let renaming = $state<number | null>(null);
  function rename(t: Track, name: string) {
    renaming = null;
    const clean = name.trim().slice(0, 60);
    if (clean !== t.name) edit((p) => setTrack(p, t.id, { name: clean }), 'Rename a track');
  }
  function focusSelect(el: HTMLInputElement) {
    el.focus();
    el.select();
  }

  let resize: { group: 'video' | 'sound'; y: number; h: number } | null = null;
  function resizeDown(e: PointerEvent, group: 'video' | 'sound') {
    if (e.button !== 0) return;
    e.stopPropagation();
    resize = { group, y: e.clientY, h: heights[group] };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function resizeMove(e: PointerEvent) {
    const r = resize;
    if (r) heights[r.group] = clampH(r.h + e.clientY - r.y);
  }
  function resizeUp() {
    resize = null;
  }
  function toggleHeight(group: 'video' | 'sound') {
    heights[group] = heights[group] < 50 ? 64 : 30;
  }

  // ---- zoom bar ----
  let barDrag: { part: 'move' | 'l' | 'r'; x: number; start: number; end: number } | null = null;
  function barDown(e: PointerEvent, part: 'move' | 'l' | 'r') {
    if (e.button !== 0) return;
    e.stopPropagation();
    barDrag = { part, x: e.clientX, start: bar01.start, end: bar01.end };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  async function barMove(e: PointerEvent) {
    const d = barDrag;
    const el = scroller;
    if (!d || !bar || !el) return;
    const df = (e.clientX - d.x) / Math.max(1, bar.clientWidth);
    if (d.part === 'move') {
      el.scrollLeft = Math.max(0, (d.start + df) * contentW);
      return;
    }
    const f0 = d.part === 'l' ? Math.min(d.start + df, d.end - 0.002) : d.start;
    const f1 = d.part === 'r' ? Math.max(d.end + df, d.start + 0.002) : d.end;
    const v = fromHandles(f0, f1, span, viewW, MAX_PPS);
    zoom = v.pps <= fit * 1.0001 ? 0 : v.pps;
    await tick();
    el.scrollLeft = v.scrollLeft;
  }
  function barUp() {
    barDrag = null;
  }
  /** A click on the bar's empty track pages to there. */
  function barTrackDown(e: PointerEvent) {
    const el = scroller;
    if (!bar || !el || e.button !== 0) return;
    const r = bar.getBoundingClientRect();
    const f = (e.clientX - r.left) / Math.max(1, r.width);
    el.scrollLeft = Math.max(0, f * contentW - viewW / 2);
  }

  // Keep the playhead in view while playing.
  $effect(() => {
    const el = scroller;
    const x = px(ed.playhead);
    if (!el || !ed.playing) return;
    if (x < el.scrollLeft || x > el.scrollLeft + viewW - 40) el.scrollLeft = Math.max(0, x - 80);
  });

  /** Thumbnails along a clip `w` × `h` px showing source `from`…`to` (ms), FilmCraft-style. */
  function slots(w: number, h: number, from: number, to: number, strip: { tiles: number; duration_ms: number }): { x: number; w: number; pos: string }[] {
    const th = h - 18;
    if (th < 14 || strip.tiles < 1 || !(strip.duration_ms > 0)) return [];
    const tw = Math.max(16, Math.round((th * 16) / 9));
    const step = Math.max(tw, w / 40);
    const out: { x: number; w: number; pos: string }[] = [];
    for (let x = 0; x < w - 2; x += step) {
      const t = from + ((x + tw / 2) / Math.max(1, w)) * (to - from);
      const tile = Math.min(strip.tiles - 1, Math.max(0, Math.floor((t / strip.duration_ms) * strip.tiles)));
      out.push({ x, w: Math.min(tw, w - x), pos: stripPosition(tile, strip.tiles) });
    }
    return out;
  }

  const rowKey = (r: TimelineRow) => (r.kind === 'track' ? `t${r.track.id}` : r.kind);
  const pressGroup = $derived(press?.started && press.zone === 'move' ? (press.track.kind === 'audio' || press.track.kind === 'music' ? 'sound' : 'video') : null);
</script>

<svelte:window onclick={() => (menu = null)} />

<div class="timeline">
  <div class="scroll" id="timeline-scroll" bind:this={scroller} use:observe use:wheelZoom onscroll={(e) => (scrollX = (e.currentTarget as HTMLElement).scrollLeft)}>
    <div class="grid" style:width="{HEADER_W + contentW}px">
      <div class="top" style:height="{TOP_H}px">
        <div class="corner" style:width="{HEADER_W}px">
          <span class="tc mono" title="Playhead">{formatTimecode(ed.playhead)}</span>
          <div class="tools">
            <button class="tool" class:on={snapping} aria-pressed={snapping} title="Snap to the playhead, clip edges and markers (hold Alt to not snap)" onclick={() => (snapping = !snapping)}>
              <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3.5 2.5v5.5a4.5 4.5 0 0 0 9 0V2.5h-3v5.5a1.5 1.5 0 0 1-3 0V2.5z" /></svg>
            </button>
            <button class="tool" title="Add a marker at the playhead (M)" onclick={() => edit((p) => addMarker(p, ed.playhead, ''), 'Add a marker')}>
              <svg viewBox="0 0 16 16" aria-hidden="true"><path class="fill" d="M4 2.5h8v8.5l-4 3-4-3z" /></svg>
            </button>
            <button class="tool" title="Fit the whole edit (double-click the zoom bar)" onclick={() => (zoom = 0)}>
              <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M1.5 8h13M1.5 8l3-3M1.5 8l3 3M14.5 8l-3-3M14.5 8l-3 3" /></svg>
            </button>
          </div>
        </div>
        <div
          class="ruler"
          style:width="{contentW}px"
          role="slider"
          tabindex="-1"
          aria-label="Playhead"
          aria-valuenow={ed.playhead}
          onpointerdown={rulerDown}
          onpointermove={rulerMove}
          onpointerup={rulerUp}
          onpointercancel={rulerUp}
        >
          {#each comp.markers as m, i (i)}
            <span class="flag {m.kind}" style:left="{px(m.at_ms)}px" title="{m.title || 'Marker'} · {formatTimecode(m.at_ms)}"></span>
          {/each}
          {#each ticks.list as t (t)}
            <span class="tick" style:left="{px(t)}px">{tickLabel(t)}</span>
          {/each}
          <span class="handle" style:left="{px(ed.playhead)}px"></span>
        </div>
      </div>

      {#snippet newRow(group: 'video' | 'sound')}
        <div class="row empty" style:height="{group === 'video' ? heights.video : heights.sound}px">
          <div class="head" style:width="{HEADER_W}px">
            <span class="chip main">{group === 'video' ? `V${videoCount + 2}` : `A${soundCount + 2}`}</span>
            <span class="name muted">{pressGroup === group ? 'Drop for a new track' : ''}</span>
          </div>
          <div
            class="lane new"
            class:drop={dropTrack === 'new' && pressGroup === group}
            data-track={group === 'video' ? 'new-video' : 'new-sound'}
            data-kind={group}
            data-pps={pps}
            style:width="{contentW}px"
            role="presentation"
            onclick={laneClick}
          ></div>
        </div>
      {/snippet}

      {#snippet clipBlock(t: Track, c: PlacedClip)}
        {@const under = overlapOf(adv, t, c)}
        <div
          class="clip {c.source.kind === 'image' ? 'image' : t.kind}"
          class:sel={ed.clipId === c.id}
          class:off={t.muted}
          style:left="{px(c.at_ms - under)}px"
          style:width="{Math.max(6, px(clipLength(c)))}px"
          role="button"
          tabindex="0"
          aria-label={sourceLabel(c.source, mainName, names)}
          onpointerdown={(e) => clipDown(e, t, c)}
          onpointermove={clipHover}
          onclick={(e) => e.stopPropagation()}
          onkeydown={(e) => {
            if (e.key === 'Enter') select(c.id);
          }}
        >
          {@render body(t.kind === 'audio' || t.kind === 'music' ? 'sound' : 'video', c.source, Math.max(6, px(clipLength(c))), t.kind === 'audio' || t.kind === 'music' ? heights.sound : heights.video, c.in_ms, c.out_ms)}
          <span class="name">{sourceLabel(c.source, mainName, names)}</span>
          {#if c.speed.length > 1}<span class="badge">ramp</span>{/if}
        </div>
      {/snippet}

      {#snippet body(kind: 'video' | 'sound', src: Source, w: number, h: number, from: number, to: number)}
        {#if kind === 'video'}
          {@const strip = thumbs(src)}
          {#if strip}
            <span class="thumbs">
              {#each slots(w, h, from, to, strip) as sl (sl.x)}
                <span class="thumb" style:left="{sl.x}px" style:width="{sl.w}px" style:background-image="url('{strip.url}')" style:background-size="{Math.round(((h - 18) * 16) / 9)}px {strip.tiles * 100}%" style:background-position-y={sl.pos}></span>
              {/each}
            </span>
          {/if}
        {:else}
          <span class="wavebox"><Waveform url={audio(src)} {from} {to} buckets={w / 2} /></span>
        {/if}
      {/snippet}

      {@render newRow('video')}
      {#each rows as r, ri (rowKey(r))}
        {@const h = rowH(r)}
        <div class="row" class:sound={r.group === 'sound'} style:height="{h}px">
          <div
            class="head"
            style:width="{HEADER_W}px"
            role="presentation"
            ondblclick={(e) => {
              if ((e.target as HTMLElement).closest('button, input, .name')) return;
              toggleHeight(r.group);
            }}
          >
            <span class="chip" class:main={r.kind !== 'track'}>{r.label}</span>
            {#if r.kind === 'track'}
              {@const t = r.track}
              <button
                class="icon"
                class:off={t.muted}
                aria-pressed={t.muted}
                title={t.muted ? 'Turn the track on' : r.group === 'video' ? 'Hide the track' : 'Mute the track'}
                onclick={() => mute(t)}
              >
                {#if r.group === 'video'}
                  <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M1.5 8s2.4-4.5 6.5-4.5S14.5 8 14.5 8 12.1 12.5 8 12.5 1.5 8 1.5 8z" /><circle class="fill" cx="8" cy="8" r="2" />{#if t.muted}<path d="M2.5 13.5l11-11" />{/if}</svg>
                {:else}M{/if}
              </button>
              {#if renaming === t.id}
                <input
                  class="rename"
                  value={t.name}
                  aria-label="Track name"
                  use:focusSelect
                  onkeydown={(e) => {
                    if (e.key === 'Enter') rename(t, (e.currentTarget as HTMLInputElement).value);
                    else if (e.key === 'Escape') renaming = null;
                  }}
                  onblur={(e) => rename(t, (e.currentTarget as HTMLInputElement).value)}
                />
              {:else}
                <span class="name" role="presentation" title="{t.name || 'Track'} (double-click to rename)" ondblclick={() => (renaming = t.id)}>{t.name || 'Track'}</span>
              {/if}
            {:else}
              <span class="icon lock" title="Main: cut it in Normal mode or the Main clip tab">
                <svg viewBox="0 0 16 16" aria-hidden="true"><rect x="3.5" y="7" width="9" height="6.5" rx="1" /><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" /></svg>
              </span>
              <span class="name" title={mainName}>{r.kind === 'main' ? mainName : 'Main sound'}</span>
            {/if}
            <span
              class="resize"
              role="separator"
              aria-orientation="horizontal"
              title="Drag to change the height of the {r.group === 'video' ? 'video' : 'sound'} tracks"
              onpointerdown={(e) => resizeDown(e, r.group)}
              onpointermove={resizeMove}
              onpointerup={resizeUp}
              onpointercancel={resizeUp}
            ></span>
          </div>
          {#if r.kind === 'track'}
            {@const t = r.track}
            <div class="lane" class:drop={dropTrack === t.id} data-track={t.id} data-kind={r.group} data-pps={pps} style:width="{contentW}px" role="presentation" onclick={laneClick}>
              {#each t.clips as c (c.id)}
                {@render clipBlock(t, c)}
              {/each}
              {#each boxes.filter((b) => b.track === t.id) as b (b.after)}
                <div
                  class="trans"
                  class:sel={isSel(b)}
                  style:left="{px(b.start)}px"
                  style:width="{Math.max(8, px(b.end - b.start))}px"
                  role="button"
                  tabindex="-1"
                  title="{KIND_LABEL[b.kind]} · {(b.ms / 1000).toFixed(2)} s (drag an end to change; right-click for more)"
                  onpointerdown={(e) => transDown(e, b, null)}
                  onclick={(e) => e.stopPropagation()}
                  oncontextmenu={(e) => transMenu(e, b)}
                  onkeydown={() => {}}
                >
                  {#if px(b.end - b.start) > 40}<span class="tname">{KIND_LABEL[b.kind]}</span>{/if}
                  <span class="tedge start" role="presentation" onpointerdown={(e) => transDown(e, b, 'start')}></span>
                  {#if b.kind === 'dip'}
                    <span class="tedge end" role="presentation" onpointerdown={(e) => transDown(e, b, 'end')}></span>
                  {/if}
                </div>
              {/each}
              {#each touching.filter((j) => j.track === t.id) as j (j.after)}
                <button class="join" style:left="{px(j.at)}px" title={j.after === FROM_BELOW ? 'Crossfade in from what is below' : 'Add a crossfade here'} onclick={(e) => {
                  e.stopPropagation();
                  addCrossfade(j);
                }}>+</button>
              {/each}
            </div>
          {:else}
            <div class="lane" style:width="{contentW}px" role="presentation" onclick={laneClick}>
              {#each segs as s, i (i)}
                <div
                  class="clip main {r.kind === 'main' ? 'video' : 'audio'}"
                  style:left="{px(s.out_start_ms)}px"
                  style:width="{Math.max(4, px(s.out_end_ms - s.out_start_ms))}px"
                  title="{mainName} · {formatTimecode(s.src_start_ms)}–{formatTimecode(s.src_end_ms)}"
                >
                  {@render body(r.kind === 'main' ? 'video' : 'sound', { kind: 'main' }, Math.max(4, px(s.out_end_ms - s.out_start_ms)), h, s.src_start_ms, s.src_end_ms)}
                  <span class="name">{mainName}</span>
                  {#if s.speed !== 1}<span class="badge">{s.speed}x</span>{/if}
                </div>
              {/each}
            </div>
          {/if}
        </div>
        {#if ri === lastVideo}
          <div class="divider" style:height="{DIVIDER_H}px" style:width="{HEADER_W + contentW}px"></div>
        {/if}
      {/each}
      {@render newRow('sound')}
      <div class="playhead" style:left="{HEADER_W + px(ed.playhead)}px"></div>
    </div>
  </div>

  <div class="zoombar">
    <span class="zb-label muted mono" style:width="{HEADER_W}px">{formatTimecode(comp.duration_ms)}</span>
    <div class="zb-track" bind:this={bar} role="presentation" onpointerdown={barTrackDown} ondblclick={() => (zoom = 0)}>
      <div
        class="zb-thumb"
        style:left="{bar01.start * 100}%"
        style:width="{Math.max(0, bar01.end - bar01.start) * 100}%"
        role="scrollbar"
        tabindex="-1"
        aria-controls="timeline-scroll"
        aria-valuenow={Math.round(bar01.start * 100)}
        onpointerdown={(e) => barDown(e, 'move')}
        onpointermove={barMove}
        onpointerup={barUp}
        onpointercancel={barUp}
      >
        <span class="zb-end l" role="presentation" onpointerdown={(e) => barDown(e, 'l')} onpointermove={barMove} onpointerup={barUp} onpointercancel={barUp}></span>
        <span class="zb-end r" role="presentation" onpointerdown={(e) => barDown(e, 'r')} onpointermove={barMove} onpointerup={barUp} onpointercancel={barUp}></span>
      </div>
    </div>
  </div>
</div>

{#if menu}
  {@const b = menu.box}
  <div
    class="menu"
    role="menu"
    tabindex="-1"
    style:left="{Math.min(menu.x, window.innerWidth - 200)}px"
    style:top="{Math.min(menu.y, window.innerHeight - 140)}px"
    onclick={(e) => e.stopPropagation()}
    onkeydown={(e) => e.key === 'Escape' && (menu = null)}
  >
    <button class="flat item" class:on={b.kind === 'crossfade'} role="menuitemradio" aria-checked={b.kind === 'crossfade'} onclick={() => setKind(b, 'crossfade')}>Crossfade</button>
    {#if b.after !== FROM_BELOW}
      <button class="flat item" class:on={b.kind === 'dip'} role="menuitemradio" aria-checked={b.kind === 'dip'} onclick={() => setKind(b, 'dip')}>Dip to black</button>
    {/if}
    <hr />
    <button class="flat item bad" role="menuitem" onclick={() => setKind(b, 'cut')}>Remove transition<kbd>Del</kbd></button>
  </div>
{/if}

<style>
  .timeline {
    /* FilmCraft's clip colours (sampled from its screenshots). */
    --tl-video: #1d4a64;
    --tl-music: #355418;
    --tl-image: #801836;
    --tl-text: #5b4a86;
    --tl-chip: #265bc1;
    --tl-clip-text: #e8eef2;
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    min-width: 0;
    overflow: hidden;
    background: var(--panel);
    font-size: var(--text-sm);
  }
  .scroll {
    flex: 1;
    min-height: 0;
    overflow: auto;
    overscroll-behavior: contain;
  }
  /* The zoom bar scrolls sideways; the vertical bar stays. */
  .scroll::-webkit-scrollbar {
    height: 0;
    width: 10px;
  }
  .scroll::-webkit-scrollbar-thumb {
    background: var(--border-strong);
    border-radius: 5px;
    border: 2px solid var(--panel);
  }
  .scroll::-webkit-scrollbar-track {
    background: transparent;
  }
  .grid {
    position: relative;
    min-height: 100%;
  }
  .top {
    position: sticky;
    top: 0;
    z-index: 4;
    display: flex;
    background: var(--panel);
    border-bottom: 1px solid var(--border);
  }
  .corner {
    position: sticky;
    left: 0;
    z-index: 5;
    flex: none;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 4px;
    padding: 0 8px;
    background: var(--panel);
    border-right: 1px solid var(--border);
  }
  .tc {
    color: var(--accent-text);
    font-size: 17px;
    line-height: 1;
  }
  .tools {
    display: flex;
    gap: 4px;
  }
  .tool {
    width: 26px;
    height: 22px;
    min-width: 0;
    padding: 0;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    background: var(--panel-raised);
    border: 1px solid var(--border);
    color: var(--text-muted);
  }
  .tool.on {
    background: var(--border-strong);
    color: var(--text-strong);
  }
  svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  svg .fill {
    fill: currentColor;
    stroke: none;
  }
  .ruler {
    position: relative;
    flex: none;
    cursor: text;
    touch-action: none;
  }
  .tick {
    position: absolute;
    bottom: 0;
    height: 30px;
    padding-left: 4px;
    border-left: 1px solid var(--border-strong);
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--text-muted);
    line-height: 14px;
    pointer-events: none;
    white-space: nowrap;
  }
  .flag {
    position: absolute;
    top: 3px;
    width: 9px;
    height: 12px;
    margin-left: -4.5px;
    background: var(--warn);
    clip-path: polygon(0 0, 100% 0, 100% 70%, 50% 100%, 0 70%);
    pointer-events: none;
  }
  .flag.bookmark {
    background: var(--accent-text);
  }
  .flag.event {
    background: var(--good);
  }
  .handle {
    position: absolute;
    bottom: 0;
    width: 13px;
    height: 18px;
    margin-left: -6.5px;
    background: var(--accent-text);
    clip-path: polygon(0 0, 100% 0, 100% 65%, 50% 100%, 0 65%);
    border-radius: 2px 2px 0 0;
    pointer-events: none;
  }
  .row {
    display: flex;
  }
  .head {
    position: sticky;
    left: 0;
    z-index: 3;
    flex: none;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 6px;
    background: var(--panel);
    border-right: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    overflow: hidden;
    user-select: none;
  }
  .chip {
    flex: none;
    min-width: 26px;
    height: 20px;
    padding: 0 4px;
    display: grid;
    place-items: center;
    border-radius: var(--radius-sm);
    background: var(--tl-chip);
    color: #fff;
    font-size: var(--text-xs);
    font-weight: 600;
  }
  .chip.main {
    background: var(--panel-raised);
    color: var(--text);
    border: 1px solid var(--border-strong);
  }
  .icon {
    flex: none;
    width: 22px;
    height: 20px;
    min-width: 0;
    padding: 0;
    display: grid;
    place-items: center;
    background: transparent;
    border: none;
    color: var(--text);
    font-size: var(--text-xs);
    font-weight: 600;
  }
  .icon.off {
    color: var(--warn);
  }
  .icon.lock {
    color: var(--text-muted);
  }
  .head .name {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text);
  }
  .rename {
    flex: 1;
    min-width: 0;
    height: 20px;
    font-size: var(--text-xs);
  }
  .resize {
    position: absolute;
    left: 0;
    right: 0;
    bottom: -2px;
    height: 5px;
    cursor: ns-resize;
    touch-action: none;
  }
  .lane {
    position: relative;
    flex: none;
    border-bottom: 1px solid var(--border);
  }
  .lane.drop {
    background: var(--hover);
  }
  .row.empty .lane {
    background: rgb(0 0 0 / 12%);
  }
  .row.empty .lane.drop {
    background: var(--hover);
    outline: 1px dashed var(--border-strong);
    outline-offset: -2px;
  }
  .divider {
    background: var(--gutter);
  }
  .clip {
    position: absolute;
    top: 1px;
    bottom: 1px;
    background: var(--tl-video);
    border: 1px solid rgb(0 0 0 / 40%);
    color: var(--tl-clip-text);
    overflow: hidden;
    display: flex;
    align-items: flex-start;
    gap: 4px;
    padding: 1px 5px;
    cursor: grab;
    touch-action: none;
    user-select: none;
  }
  .clip.music {
    background: var(--tl-music);
  }
  .clip.image {
    background: var(--tl-image);
  }
  .clip.text {
    background: var(--tl-text);
  }
  .clip.main {
    cursor: default;
  }
  .clip.sel {
    outline: 2px solid #fff;
    outline-offset: -2px;
    z-index: 1;
  }
  .clip.off {
    opacity: 0.45;
  }
  .thumbs,
  .wavebox {
    position: absolute;
    left: 0;
    right: 0;
    top: 17px;
    bottom: 0;
    overflow: hidden;
    pointer-events: none;
  }
  .thumb {
    position: absolute;
    top: 0;
    bottom: 0;
    background-repeat: no-repeat;
    border-right: 1px solid rgb(0 0 0 / 50%);
  }
  .clip .name {
    position: relative;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: var(--text-xs);
    line-height: 15px;
  }
  .badge {
    position: relative;
    flex: none;
    font-size: 10px;
    line-height: 14px;
    padding: 0 3px;
    border-radius: 3px;
    background: rgb(0 0 0 / 40%);
  }
  .trans {
    position: absolute;
    top: 1px;
    bottom: 1px;
    z-index: 2;
    background:
      repeating-linear-gradient(135deg, rgb(217 217 217 / 45%) 0 1px, transparent 1px 4px),
      rgb(0 0 0 / 35%);
    border: 1px solid #ebebeb;
    cursor: pointer;
    touch-action: none;
  }
  .trans.sel {
    border: 2px solid var(--accent-text);
  }
  .tname {
    position: absolute;
    top: 2px;
    left: 2px;
    max-width: calc(100% - 4px);
    padding: 0 3px;
    background: rgb(0 0 0 / 67%);
    color: #d9d9d9;
    font-size: 10px;
    line-height: 13px;
    white-space: nowrap;
    overflow: hidden;
    pointer-events: none;
  }
  .tedge {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 6px;
    cursor: ew-resize;
  }
  .tedge.start {
    left: -2px;
  }
  .tedge.end {
    right: -2px;
  }
  .join {
    position: absolute;
    top: 50%;
    z-index: 2;
    width: 18px;
    height: 18px;
    min-width: 0;
    margin: -9px 0 0 -9px;
    padding: 0;
    border-radius: 50%;
    display: grid;
    place-items: center;
    background: var(--accent);
    color: var(--on-accent);
    border: 1px solid #fff;
    font-size: 13px;
    line-height: 1;
    opacity: 0;
    transition: opacity var(--motion);
  }
  .lane:hover .join,
  .join:focus-visible {
    opacity: 1;
  }
  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    z-index: 2;
    width: 1px;
    background: var(--accent-text);
    pointer-events: none;
  }
  .zoombar {
    flex: none;
    display: flex;
    align-items: center;
    height: 18px;
    border-top: 1px solid var(--border);
    background: var(--panel);
  }
  .zb-label {
    flex: none;
    padding: 0 8px;
    font-size: 10px;
  }
  .zb-track {
    position: relative;
    flex: 1;
    height: 10px;
    margin: 0 12px;
  }
  .zb-thumb {
    position: absolute;
    top: 0;
    bottom: 0;
    min-width: 20px;
    border-radius: 5px;
    background: #4b4b4b;
    cursor: grab;
    touch-action: none;
  }
  .zb-end {
    position: absolute;
    top: -2px;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 1.5px solid #d9d9d9;
    background: var(--panel);
    cursor: ew-resize;
  }
  .zb-end.l {
    left: -7px;
  }
  .zb-end.r {
    right: -7px;
  }
  .menu {
    position: fixed;
    z-index: 20;
    min-width: 190px;
    background: var(--panel-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 4px;
    box-shadow: 0 8px 24px rgb(0 0 0 / 35%);
    display: flex;
    flex-direction: column;
  }
  .item {
    justify-content: space-between;
    color: var(--text-strong);
  }
  .item.on::after {
    content: '✓';
  }
  .item.bad {
    color: var(--bad);
  }
  .menu hr {
    width: 100%;
    border: none;
    border-top: 1px solid var(--border);
    margin: 4px 0;
  }
  kbd {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--text-muted);
  }
  .mono {
    font-family: var(--font-mono);
  }
  .muted {
    color: var(--text-muted);
  }
</style>
