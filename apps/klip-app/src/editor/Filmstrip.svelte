<!--
  The filmstrip: the clip's SOURCE timeline as its hover-scrub tiles, with the parts
  the edit throws away shaded, speed badges, bookmarks, the trim handles at both ends
  of the kept range, the playhead, a drag selection, and the text cues on a lane below
  (drag to move). Pointer maths in source ms; the parent gets output times.
-->
<script lang="ts">
  import { gaps, msToPct, pxToMs, speedBadge } from '../lib/editor';
  import type { SourceInfo } from '../lib/editor';
  import { formatTimecode } from '../lib/format';
  import { stripPosition } from '../lib/library';
  import type { Project } from '../lib/timeline';
  import { outputDurationMs, outputTime, pieces, sourceTime } from '../lib/timeline';

  let {
    strip,
    tiles,
    source,
    project,
    playhead,
    selection,
    textId,
    onseek,
    onselect,
    ontrimstart,
    ontrimend,
    ontextmove,
    onpicktext,
  }: {
    strip: string | null;
    tiles: number;
    source: SourceInfo;
    project: Project;
    playhead: number;
    selection: { start: number; end: number } | null;
    textId: number | null;
    onseek: (outMs: number) => void;
    onselect: (startOut: number, endOut: number) => void;
    ontrimstart: (srcMs: number) => void;
    ontrimend: (srcMs: number) => void;
    ontextmove: (id: number, startOut: number) => void;
    onpicktext: (id: number | null) => void;
  } = $props();

  const duration = $derived(Math.max(1, source.duration_ms));
  const kept = $derived(pieces(project));
  const first = $derived(kept[0]?.start_ms ?? 0);
  const last = $derived(kept[kept.length - 1]?.end_ms ?? duration);
  const playSrc = $derived(sourceTime(project, playhead) ?? first);
  const outDuration = $derived(outputDurationMs(project));

  /** Output time of a source time, or of the nearest kept time after it. */
  function toOut(srcMs: number): number {
    const direct = outputTime(project, srcMs);
    if (direct !== null) return direct;
    for (const p of kept) {
      if (p.start_ms >= srcMs) return outputTime(project, p.start_ms) ?? 0;
    }
    return outDuration;
  }

  /** Source span of an output span (for drawing). */
  function toSrc(outMs: number): number {
    return sourceTime(project, Math.max(0, Math.min(outMs, outDuration - 1))) ?? last;
  }

  let strap = $state<HTMLDivElement | null>(null);
  let drag = $state<
    | { kind: 'select'; a: number; b: number }
    | { kind: 'start'; at: number }
    | { kind: 'end'; at: number }
    | { kind: 'cue'; id: number; offset: number; at: number; length: number }
    | null
  >(null);

  function srcAt(e: PointerEvent): number {
    const r = strap?.getBoundingClientRect();
    if (!r) return 0;
    return pxToMs(e.clientX - r.left, r.width, duration);
  }

  function down(e: PointerEvent, kind: 'select' | 'start' | 'end') {
    if (e.button !== 0) return;
    e.stopPropagation();
    const at = srcAt(e);
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    if (kind === 'select') {
      drag = { kind, a: at, b: at };
      onseek(toOut(at));
    } else drag = { kind, at };
  }

  function cueDown(e: PointerEvent, id: number, startOut: number, endOut: number) {
    if (e.button !== 0) return;
    e.stopPropagation();
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    onpicktext(id);
    const at = srcAt(e);
    drag = { kind: 'cue', id, offset: at - toSrc(startOut), at: toSrc(startOut), length: endOut - startOut };
  }

  function move(e: PointerEvent) {
    if (!drag) return;
    const at = srcAt(e);
    if (drag.kind === 'select') drag = { ...drag, b: at };
    else if (drag.kind === 'cue') drag = { ...drag, at: at - drag.offset };
    else drag = { ...drag, at };
  }

  function up() {
    const d = drag;
    drag = null;
    if (!d) return;
    if (d.kind === 'select') {
      if (Math.abs(d.b - d.a) < duration / 200) onselect(0, 0);
      else onselect(toOut(Math.min(d.a, d.b)), toOut(Math.max(d.a, d.b)));
    } else if (d.kind === 'start') ontrimstart(Math.min(d.at, last - 1));
    else if (d.kind === 'end') ontrimend(Math.max(d.at, first + 1));
    else ontextmove(d.id, Math.max(0, Math.min(outDuration - d.length, toOut(d.at))));
  }

  const startX = $derived(drag?.kind === 'start' ? drag.at : first);
  const endX = $derived(drag?.kind === 'end' ? drag.at : last);
  const sel = $derived.by(() => {
    if (drag?.kind === 'select') return { a: Math.min(drag.a, drag.b), b: Math.max(drag.a, drag.b) };
    if (selection) return { a: toSrc(selection.start), b: toSrc(selection.end - 1) + 1 };
    return null;
  });
</script>

<div class="filmstrip">
  <div
    class="strap"
    bind:this={strap}
    role="presentation"
    onpointerdown={(e) => down(e, 'select')}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={up}
  >
    {#each Array.from({ length: tiles }, (_, i) => i) as i (i)}
      <div
        class="tile"
        style:background-image={strip ? `url(${strip})` : undefined}
        style:background-size="100% {tiles * 100}%"
        style:background-position-y={stripPosition(i, tiles)}
      ></div>
    {/each}
    {#each gaps(project) as g (g.start_ms)}
      <div class="gap" style:left="{msToPct(g.start_ms, duration)}%" style:width="{msToPct(g.end_ms - g.start_ms, duration)}%"></div>
    {/each}
    {#each kept as p, i (i)}
      {#if speedBadge(p.speed)}
        <span class="speed mono" style:left="{msToPct(p.start_ms, duration)}%">{speedBadge(p.speed)}</span>
      {/if}
    {/each}
    {#each source.bookmarks_ms as b (b)}
      <span class="mark" style:left="{msToPct(b, duration)}%" title="Bookmark at {formatTimecode(b)}">◆</span>
    {/each}
    {#if sel && sel.b > sel.a}
      <div class="sel" style:left="{msToPct(sel.a, duration)}%" style:width="{msToPct(sel.b - sel.a, duration)}%"></div>
    {/if}
    <div class="playhead" style:left="{msToPct(playSrc, duration)}%"></div>
    <div
      class="handle start"
      role="slider"
      aria-label="Start of the kept range"
      aria-valuenow={Math.round(startX)}
      tabindex="0"
      style:left="{msToPct(startX, duration)}%"
      onpointerdown={(e) => down(e, 'start')}
      onpointermove={move}
      onpointerup={up}
      onpointercancel={up}
    ></div>
    <div
      class="handle end"
      role="slider"
      aria-label="End of the kept range"
      aria-valuenow={Math.round(endX)}
      tabindex="0"
      style:left="{msToPct(endX, duration)}%"
      onpointerdown={(e) => down(e, 'end')}
      onpointermove={move}
      onpointerup={up}
      onpointercancel={up}
    ></div>
  </div>
  <div class="lane" aria-label="Text cues">
    {#each project.text as t (t.id)}
      {@const a = drag?.kind === 'cue' && drag.id === t.id ? drag.at : toSrc(t.start_ms)}
      {@const b = a + (toSrc(t.end_ms - 1) + 1 - toSrc(t.start_ms))}
      <button
        class="cue"
        class:on={textId === t.id}
        title="{t.text} ({formatTimecode(t.start_ms)} – {formatTimecode(t.end_ms)})"
        style:left="{msToPct(a, duration)}%"
        style:width="{Math.max(1.5, msToPct(b - a, duration))}%"
        onpointerdown={(e) => cueDown(e, t.id, t.start_ms, t.end_ms)}
        onpointermove={move}
        onpointerup={up}
        onpointercancel={up}
      >
        {t.text}
      </button>
    {/each}
  </div>
  <div class="scale mono">
    <span>{formatTimecode(0)}</span>
    <span class="muted">source {formatTimecode(duration)} · edit {formatTimecode(outDuration)}</span>
    <span>{formatTimecode(duration)}</span>
  </div>
</div>

<style>
  .filmstrip {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    user-select: none;
  }
  .strap {
    position: relative;
    display: flex;
    height: 72px;
    background: var(--viewer);
    border-radius: var(--radius-sm);
    overflow: hidden;
    touch-action: none;
    cursor: crosshair;
  }
  .tile {
    flex: 1;
    min-width: 0;
    background-repeat: no-repeat;
    background-color: var(--panel-raised);
    border-right: 1px solid rgb(0 0 0 / 40%);
  }
  .gap,
  .sel,
  .playhead,
  .handle,
  .speed,
  .mark {
    position: absolute;
    top: 0;
    bottom: 0;
  }
  .gap {
    background: repeating-linear-gradient(135deg, rgb(0 0 0 / 70%) 0 6px, rgb(0 0 0 / 55%) 6px 12px);
    pointer-events: none;
  }
  .sel {
    background: color-mix(in srgb, var(--accent) 28%, transparent);
    border-left: 1px solid var(--accent);
    border-right: 1px solid var(--accent);
    pointer-events: none;
  }
  .playhead {
    width: 2px;
    margin-left: -1px;
    background: var(--text-strong);
    pointer-events: none;
  }
  .handle {
    width: 10px;
    margin-left: -5px;
    background: var(--accent);
    cursor: ew-resize;
    border-radius: 2px;
  }
  .handle:focus-visible {
    outline: 2px solid var(--focus);
  }
  .speed {
    bottom: auto;
    top: 2px;
    padding: 0 4px;
    margin-left: 2px;
    font-size: var(--text-xs);
    color: var(--on-accent);
    background: var(--accent);
    border-radius: var(--radius-sm);
    pointer-events: none;
  }
  .mark {
    top: auto;
    bottom: 0;
    font-size: calc(10px * var(--text-scale));
    color: var(--warn);
    margin-left: -5px;
    pointer-events: none;
  }
  .lane {
    position: relative;
    height: 22px;
    background: var(--panel-raised);
    border-radius: var(--radius-sm);
  }
  .cue {
    position: absolute;
    top: 2px;
    bottom: 2px;
    height: auto;
    min-width: 0;
    padding: 0 6px;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--panel);
    font-size: var(--text-xs);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    cursor: grab;
    touch-action: none;
  }
  .cue.on {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 22%, var(--panel));
  }
  .scale {
    display: flex;
    justify-content: space-between;
    font-size: var(--text-xs);
    color: var(--text-muted);
  }
</style>
