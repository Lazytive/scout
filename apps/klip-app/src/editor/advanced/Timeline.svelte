<!--
  Advanced mode's timeline: a ruler with the markers, the Normal-mode cut of the main
  clip as track "Main", the Advanced tracks (video on top in draw order, sound below),
  and under the selected clip its key lanes (transform, opacity, volume) and speed
  points. Clips drag along and across tracks of their kind and trim at their edges;
  everything snaps to the playhead, clip edges and markers (hold Alt to not snap).
  Click a lane to add a key there; drag a key to move it; double-click it to remove it.
-->
<script lang="ts">
  import type { PlacedClip, Track } from '../../lib/advanced';
  import {
    addKey,
    addMarker,
    clipEnd,
    clipLength,
    fitsTrack,
    moveClip,
    moveKey,
    readAdvanced,
    removeKey,
    removeSpeedPoint,
    setSpeedPoint,
    rateAt,
    sourceLabel,
    trimClipEnd,
    trimClipStart,
  } from '../../lib/advanced';
  import type { Composition } from '../../lib/compose';
  import { beginGesture, ed, edit, endGesture, gesture, seek } from '../../lib/editor.svelte';
  import { formatTimecode } from '../../lib/format';
  import type { Project } from '../../lib/timeline';
  import { segments } from '../../lib/timeline';

  let {
    comp,
    project,
    mainName,
    names,
    sourceMs,
  }: {
    comp: Composition;
    project: Project;
    mainName: string;
    /** Library clip names by key. */
    names: Map<string, string>;
    /** A source's length in ms (to bound trims), or 0 when unknown. */
    sourceMs: (c: PlacedClip) => number;
  } = $props();

  const HEADER_W = 132;
  const ROW_H = 40;
  const LANE_H = 22;
  const SNAP_PX = 8;

  let scroller = $state<HTMLDivElement | null>(null);
  let viewW = $state(800);
  /** Pixels per second; 0 = fit the whole edit. */
  let zoom = $state(0);
  let snapping = $state(true);

  const adv = $derived(readAdvanced(project));
  const duration = $derived(Math.max(comp.duration_ms, 1000));
  const fitPps = $derived(Math.max(4, (viewW - 24) / (duration / 1000)));
  const pps = $derived(zoom > 0 ? zoom : fitPps);
  const width = $derived(Math.ceil((duration / 1000) * pps) + 80);
  const px = (ms: number) => (ms / 1000) * pps;
  const msAt = (x: number) => Math.max(0, Math.round((x / pps) * 1000));

  const segs = $derived(segments(project));
  /** Rows top to bottom: video tracks from the top of the picture down, Main, then sound. */
  const videoTracks = $derived([...adv.tracks].filter((t) => t.kind === 'video' || t.kind === 'text').reverse());
  const soundTracks = $derived(adv.tracks.filter((t) => t.kind === 'audio' || t.kind === 'music'));
  const selected = $derived.by(() => {
    for (const t of adv.tracks) {
      const c = t.clips.find((x) => x.id === ed.clipId);
      if (c) return { track: t, clip: c };
    }
    return null;
  });

  function observe(el: HTMLDivElement) {
    const ro = new ResizeObserver(() => (viewW = el.clientWidth));
    ro.observe(el);
    viewW = el.clientWidth;
    return { destroy: () => ro.disconnect() };
  }

  // Ruler ticks: a step that keeps labels ~90 px apart.
  const ticks = $derived.by(() => {
    const steps = [100, 250, 500, 1000, 2000, 5000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000, 600_000];
    const step = steps.find((s) => px(s) >= 90) ?? 600_000;
    const out: number[] = [];
    for (let t = 0; t <= duration; t += step) out.push(t);
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
    scrubbing = true;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    rulerMove(e);
  }
  function rulerMove(e: PointerEvent) {
    if (!scrubbing) return;
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    ed.playing = false;
    seek(snap(msAt(e.clientX - r.left), snapTargets(null), e.altKey));
  }
  function rulerUp() {
    scrubbing = false;
  }

  // ---- clips: move and trim ----
  type Drag = { kind: 'move' | 'start' | 'end'; clip: PlacedClip; track: Track; x: number; y: number; at: number; end: number; targets: number[] };
  let drag = $state<Drag | null>(null);
  let dropTrack = $state<number | 'new' | null>(null);

  function clipDown(e: PointerEvent, track: Track, clip: PlacedClip, kind: Drag['kind']) {
    e.stopPropagation();
    ed.clipId = clip.id;
    ed.textId = null;
    drag = { kind, clip, track, x: e.clientX, y: e.clientY, at: clip.at_ms, end: clipEnd(clip), targets: snapTargets(clip.id) };
    beginGesture();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
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
    const d = drag;
    if (!d) return;
    const dms = ((e.clientX - d.x) / pps) * 1000;
    if (d.kind === 'move') {
      const len = d.end - d.at;
      const at = Math.max(0, snapMove(Math.round(d.at + dms), len, d.targets, e.altKey));
      const target = Math.abs(e.clientY - d.y) > ROW_H / 2 ? trackUnder(e, d.clip) : null;
      dropTrack = target;
      gesture((p) => moveClip(p, d.clip.id, at, target === null ? undefined : target));
    } else if (d.kind === 'start') {
      const at = snap(Math.round(d.at + dms), d.targets, e.altKey);
      gesture((p) => trimClipStart(p, d.clip.id, at));
    } else {
      const end = snap(Math.round(d.end + dms), d.targets, e.altKey);
      gesture((p) => trimClipEnd(p, d.clip.id, end, sourceMs(d.clip)));
    }
  }

  function clipUp() {
    const d = drag;
    drag = null;
    dropTrack = null;
    if (!d) return;
    endGesture(d.kind === 'move' ? 'Move clip' : 'Trim clip');
  }

  // ---- lanes ----
  type LaneId = 'transform' | 'opacity' | 'volume' | 'speed';
  const LANES: { id: LaneId; label: string }[] = [
    { id: 'transform', label: 'Zoom / position' },
    { id: 'opacity', label: 'Opacity' },
    { id: 'volume', label: 'Volume' },
    { id: 'speed', label: 'Speed' },
  ];

  function laneTimes(c: PlacedClip, lane: LaneId): number[] {
    if (lane === 'transform') return c.transform.map((k) => k.at_ms);
    if (lane === 'speed') return c.speed.map((s) => s.at_ms);
    return c[lane].map((k) => k.at_ms);
  }

  const laneFor = (lane: LaneId) => (lane === 'transform' ? 'zoom' : lane === 'speed' ? null : lane);

  function laneClick(e: MouseEvent, c: PlacedClip, lane: LaneId) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const local = Math.min(clipLength(c), Math.max(0, msAt(e.clientX - r.left) - c.at_ms));
    seek(c.at_ms + local);
    if (lane === 'speed') edit((p) => setSpeedPoint(p, c.id, local, rateAt(c, local)), 'Add a speed point');
    else edit((p) => addKey(p, c.id, laneFor(lane)!, local), 'Add a key');
  }

  let keyDrag: { clip: PlacedClip; lane: LaneId; from: number; x: number; moved: boolean } | null = null;
  function keyDown(e: PointerEvent, c: PlacedClip, lane: LaneId, at: number) {
    e.stopPropagation();
    keyDrag = { clip: c, lane, from: at, x: e.clientX, moved: false };
    beginGesture();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function keyMove(e: PointerEvent) {
    const k = keyDrag;
    if (!k) return;
    const dms = Math.round(((e.clientX - k.x) / pps) * 1000);
    if (Math.abs(e.clientX - k.x) > 2) k.moved = true;
    const to = Math.min(clipLength(k.clip), Math.max(0, k.from + dms));
    if (k.lane === 'speed') {
      if (k.from === 0) return;
      const rate = rateAt(k.clip, k.from);
      gesture((p) => {
        removeSpeedPoint(p, k.clip.id, k.from);
        setSpeedPoint(p, k.clip.id, to, rate);
      });
    } else gesture((p) => moveKey(p, k.clip.id, laneFor(k.lane)!, k.from, to));
  }
  function keyUp(e: PointerEvent) {
    const k = keyDrag;
    keyDrag = null;
    if (!k) return;
    if (!k.moved) {
      endGesture('Move a key');
      seek(k.clip.at_ms + k.from);
      ed.playing = false;
      return;
    }
    void e;
    endGesture(k.lane === 'speed' ? 'Move a speed point' : 'Move a key');
  }
  function keyRemove(c: PlacedClip, lane: LaneId, at: number) {
    if (lane === 'speed') edit((p) => removeSpeedPoint(p, c.id, at), 'Remove a speed point');
    else edit((p) => removeKey(p, c.id, laneFor(lane)!, at), 'Remove a key');
  }

  function rowClick(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    ed.clipId = null;
    ed.playing = false;
    seek(snap(msAt(e.clientX - r.left), snapTargets(null), e.altKey));
  }

  const kindLabel: Record<string, string> = { video: 'V', text: 'T', audio: 'S', music: 'M' };

  // Keep the playhead in view while playing.
  $effect(() => {
    const el = scroller;
    const x = px(ed.playhead);
    if (!el || !ed.playing) return;
    if (x < el.scrollLeft || x > el.scrollLeft + el.clientWidth - 40) el.scrollLeft = Math.max(0, x - 80);
  });

  function setTrack(t: Track, patch: Partial<Pick<Track, 'muted'>>) {
    edit((p) => {
      const a = readAdvanced(p);
      const x = a.tracks.find((y) => y.id === t.id);
      if (x && patch.muted !== undefined) x.muted = patch.muted;
      p.advanced = a;
    }, patch.muted ? 'Mute a track' : 'Unmute a track');
  }
</script>

<div class="timeline">
  <div class="toolbar">
    <button class="flat" title="Fit the whole edit" onclick={() => (zoom = 0)}>Fit</button>
    <label class="zoom">
      <span class="muted">Zoom</span>
      <input
        type="range"
        min="4"
        max="400"
        step="1"
        value={Math.round(pps)}
        aria-label="Timeline zoom"
        oninput={(e) => (zoom = Number((e.currentTarget as HTMLInputElement).value))}
      />
    </label>
    <button class="flat" class:on={snapping} aria-pressed={snapping} title="Snap to the playhead, clip edges and markers (hold Alt to not snap)" onclick={() => (snapping = !snapping)}>
      Snap
    </button>
    <button class="flat" title="Add a marker at the playhead (M)" onclick={() => edit((p) => addMarker(p, ed.playhead, ''), 'Add a marker')}>+ Marker</button>
    <span class="grow"></span>
    <span class="muted mono">{formatTimecode(ed.playhead)} / {formatTimecode(comp.duration_ms)}</span>
  </div>
  <div class="body">
    <div class="heads" style:width="{HEADER_W}px">
      <div class="ruler-head"></div>
      {#each videoTracks as t (t.id)}
        <div class="head" style:height="{ROW_H}px">
          <span class="kind">{kindLabel[t.kind]}</span>
          <span class="name" title={t.name}>{t.name || 'Track'}</span>
          <button class="flat tiny" class:on={t.muted} aria-pressed={t.muted} title="Mute" onclick={() => setTrack(t, { muted: !t.muted })}>M</button>
        </div>
        {#if selected && selected.track.id === t.id}
          {#each LANES.filter((l) => l.id !== 'volume' || selected.clip.source.kind !== 'image').filter((l) => l.id !== 'speed' || selected.clip.source.kind !== 'image') as l (l.id)}
            <div class="lane-head" style:height="{LANE_H}px">{l.label}</div>
          {/each}
        {/if}
      {/each}
      <div class="head main" style:height="{ROW_H}px"><span class="kind">V</span><span class="name">Main</span></div>
      {#each soundTracks as t (t.id)}
        <div class="head" style:height="{ROW_H}px">
          <span class="kind">{kindLabel[t.kind]}</span>
          <span class="name" title={t.name}>{t.name || 'Track'}</span>
          <button class="flat tiny" class:on={t.muted} aria-pressed={t.muted} title="Mute" onclick={() => setTrack(t, { muted: !t.muted })}>M</button>
        </div>
        {#if selected && selected.track.id === t.id}
          {#each LANES.filter((l) => l.id === 'volume' || l.id === 'speed') as l (l.id)}
            <div class="lane-head" style:height="{LANE_H}px">{l.label}</div>
          {/each}
        {/if}
      {/each}
      {#if drag?.kind === 'move'}
        <div class="head new" style:height="{ROW_H}px">New track</div>
      {/if}
    </div>
    <div class="scroll" bind:this={scroller} use:observe>
      <div class="content" style:width="{width}px">
        <div
          class="ruler"
          role="slider"
          tabindex="-1"
          aria-label="Playhead"
          aria-valuenow={ed.playhead}
          onpointerdown={rulerDown}
          onpointermove={rulerMove}
          onpointerup={rulerUp}
          onpointercancel={rulerUp}
        >
          {#each ticks.list as t (t)}
            <span class="tick" style:left="{px(t)}px">{tickLabel(t)}</span>
          {/each}
          {#each comp.markers as m, i (i)}
            <span class="marker {m.kind}" style:left="{px(m.at_ms)}px" title="{m.title} · {formatTimecode(m.at_ms)}"></span>
          {/each}
        </div>
        {#snippet clipRow(t: Track)}
          <div
            class="row"
            class:drop={dropTrack === t.id}
            data-track={t.id}
            style:height="{ROW_H}px"
            role="presentation"
            onclick={rowClick}
          >
            {#each t.clips as c (c.id)}
              {@const len = clipLength(c)}
              <div
                class="clip {t.kind}"
                class:sel={ed.clipId === c.id}
                class:muted-track={t.muted}
                style:left="{px(c.at_ms)}px"
                style:width="{Math.max(6, px(len))}px"
                role="button"
                tabindex="0"
                aria-label={sourceLabel(c.source, mainName, names)}
                onpointerdown={(e) => clipDown(e, t, c, 'move')}
                onpointermove={clipMove}
                onpointerup={clipUp}
                onpointercancel={clipUp}
                onclick={(e) => e.stopPropagation()}
                onkeydown={(e) => {
                  if (e.key === 'Enter') ed.clipId = c.id;
                }}
              >
                <span class="label">{sourceLabel(c.source, mainName, names)}</span>
                {#if c.speed.length}<span class="badge">ramp</span>{/if}
                {#if adv.transitions.some((x) => x.track === t.id && x.after_clip === c.id)}
                  <span class="badge tr" title="Transition into the next clip">⇢</span>
                {/if}
                <span
                  class="edge start"
                  role="presentation"
                  onpointerdown={(e) => clipDown(e, t, c, 'start')}
                  onpointermove={clipMove}
                  onpointerup={clipUp}
                  onpointercancel={clipUp}
                ></span>
                <span
                  class="edge end"
                  role="presentation"
                  onpointerdown={(e) => clipDown(e, t, c, 'end')}
                  onpointermove={clipMove}
                  onpointerup={clipUp}
                  onpointercancel={clipUp}
                ></span>
              </div>
            {/each}
          </div>
          {#if selected && selected.track.id === t.id}
            {@const c = selected.clip}
            {#each LANES.filter((l) => (t.kind === 'video' ? (c.source.kind === 'image' ? l.id === 'transform' || l.id === 'opacity' : true) : l.id === 'volume' || l.id === 'speed')) as l (l.id)}
              <div class="lane" style:height="{LANE_H}px">
                <div
                  class="lane-span"
                  role="button"
                  tabindex="-1"
                  title="Click to add a key"
                  style:left="{px(c.at_ms)}px"
                  style:width="{px(clipLength(c))}px"
                  onclick={(e) => laneClick(e, c, l.id)}
                  onkeydown={() => {}}
                >
                  {#each laneTimes(c, l.id) as at (at)}
                    <span
                      class="key"
                      class:speed={l.id === 'speed'}
                      style:left="{px(at)}px"
                      role="button"
                      tabindex="-1"
                      title={l.id === 'speed' ? `${rateAt(c, at)} % from ${formatTimecode(at)}` : formatTimecode(at)}
                      onpointerdown={(e) => keyDown(e, c, l.id, at)}
                      onpointermove={keyMove}
                      onpointerup={keyUp}
                      onpointercancel={keyUp}
                      onclick={(e) => e.stopPropagation()}
                      ondblclick={(e) => {
                        e.stopPropagation();
                        keyRemove(c, l.id, at);
                      }}
                      onkeydown={() => {}}
                    >{l.id === 'speed' ? `${rateAt(c, at)}%` : ''}</span>
                  {/each}
                </div>
              </div>
            {/each}
          {/if}
        {/snippet}
        {#each videoTracks as t (t.id)}
          {@render clipRow(t)}
        {/each}
        <div class="row main" style:height="{ROW_H}px" role="presentation" onclick={rowClick}>
          {#each segs as s, i (i)}
            <div class="clip video main-seg" style:left="{px(s.out_start_ms)}px" style:width="{Math.max(4, px(s.out_end_ms - s.out_start_ms))}px" title="{mainName} · {formatTimecode(s.src_start_ms)}–{formatTimecode(s.src_end_ms)}">
              <span class="label">{mainName}</span>
              {#if s.speed !== 1}<span class="badge">{s.speed}x</span>{/if}
            </div>
          {/each}
        </div>
        {#each soundTracks as t (t.id)}
          {@render clipRow(t)}
        {/each}
        {#if drag?.kind === 'move'}
          <div class="row new" class:drop={dropTrack === 'new'} data-track={selected && (selected.track.kind === 'audio' || selected.track.kind === 'music') ? 'new-sound' : 'new-video'} style:height="{ROW_H}px"></div>
        {/if}
        <div class="playhead" style:left="{px(ed.playhead)}px"></div>
      </div>
    </div>
  </div>
</div>

<style>
  .timeline {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    min-width: 0;
    overflow: hidden;
    background: var(--panel);
    font-size: var(--text-sm);
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 4px var(--space-2);
    border-bottom: 1px solid var(--border);
  }
  .toolbar .on,
  .tiny.on {
    color: var(--accent-text);
  }
  .zoom {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .zoom input {
    width: 120px;
  }
  .grow {
    flex: 1;
  }
  .body {
    display: flex;
    flex: 1;
    min-height: 0;
    min-width: 0;
    overflow-y: auto;
  }
  .heads {
    flex: none;
    border-right: 1px solid var(--border);
    background: var(--chrome);
  }
  .ruler-head {
    height: 24px;
    border-bottom: 1px solid var(--border);
  }
  .head,
  .lane-head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 6px;
    border-bottom: 1px solid var(--border);
    overflow: hidden;
  }
  .lane-head {
    color: var(--text-muted);
    font-size: var(--text-xs);
    padding-left: 22px;
  }
  .head.new {
    color: var(--text-muted);
    font-style: italic;
  }
  .kind {
    font-family: var(--font-mono);
    color: var(--text-muted);
    width: 12px;
  }
  .name {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tiny {
    padding: 0 4px;
    min-width: 0;
    height: 20px;
    font-size: var(--text-xs);
  }
  .scroll {
    flex: 1;
    overflow-x: auto;
    overflow-y: hidden;
    min-width: 0;
  }
  .content {
    position: relative;
  }
  .ruler {
    position: relative;
    height: 24px;
    border-bottom: 1px solid var(--border);
    cursor: text;
    background: var(--chrome);
  }
  .tick {
    position: absolute;
    top: 0;
    bottom: 0;
    padding-left: 3px;
    border-left: 1px solid var(--border-strong);
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--text-muted);
    line-height: 22px;
    pointer-events: none;
  }
  .marker {
    position: absolute;
    top: 2px;
    width: 8px;
    height: 8px;
    margin-left: -4px;
    transform: rotate(45deg);
    background: var(--warn);
  }
  .marker.bookmark {
    background: var(--accent-text);
  }
  .marker.event {
    background: var(--good);
  }
  .row {
    position: relative;
    border-bottom: 1px solid var(--border);
  }
  .row.drop {
    background: var(--hover);
  }
  .row.new {
    border-bottom-style: dashed;
  }
  .clip {
    position: absolute;
    top: 3px;
    bottom: 3px;
    border-radius: var(--radius-sm);
    background: #2b4f7e;
    border: 1px solid #3d6aa6;
    color: var(--text-strong);
    overflow: hidden;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 6px;
    cursor: grab;
    touch-action: none;
    user-select: none;
  }
  .clip.audio {
    background: #2d5b3f;
    border-color: #3e7d56;
  }
  .clip.music {
    background: #5b3f6e;
    border-color: #7a5592;
  }
  .clip.main-seg {
    background: #3a3a3a;
    border-color: #555;
    cursor: default;
  }
  .clip.sel {
    outline: 2px solid var(--accent-text);
    outline-offset: -1px;
  }
  .clip.muted-track {
    opacity: 0.55;
  }
  .label {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: var(--text-xs);
  }
  .badge {
    font-size: 10px;
    padding: 0 3px;
    border-radius: 3px;
    background: rgb(0 0 0 / 35%);
  }
  .edge {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 7px;
    cursor: ew-resize;
  }
  .edge.start {
    left: 0;
  }
  .edge.end {
    right: 0;
  }
  .lane {
    position: relative;
    border-bottom: 1px solid var(--border);
    background: rgb(0 0 0 / 15%);
  }
  .lane-span {
    position: absolute;
    top: 2px;
    bottom: 2px;
    background: var(--hover);
    border-radius: 3px;
    cursor: copy;
  }
  .key {
    position: absolute;
    top: 50%;
    width: 9px;
    height: 9px;
    margin: -5px 0 0 -5px;
    transform: rotate(45deg);
    background: var(--accent-text);
    border: 1px solid var(--text-strong);
    cursor: ew-resize;
    touch-action: none;
  }
  .key.speed {
    transform: none;
    width: auto;
    height: auto;
    margin: -8px 0 0 -2px;
    padding: 0 3px;
    font-size: 10px;
    border-radius: 3px;
    background: var(--warn);
    color: #000;
    border: none;
  }
  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--rec);
    pointer-events: none;
  }
  .mono {
    font-family: var(--font-mono);
  }
  .muted {
    color: var(--text-muted);
  }
</style>
