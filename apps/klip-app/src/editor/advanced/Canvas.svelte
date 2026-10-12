<!--
  The Advanced preview: the composition drawn on a canvas by `Player` (the export's
  placement math, opacity, the colour grade through an SVG feColorMatrix, text cues),
  its sound through `SoundGraph`. The selected clip shows a frame: drag it to move the
  picture, scroll on it to zoom (both at the playhead).
-->
<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  import type { Composition, Layer } from '../../lib/compose';
  import { isIdentityColour, svgColourMatrix } from '../../lib/keyframes';
  import type { Cue, Resolve } from './player';
  import { Player } from './player';

  let {
    comp,
    resolve,
    playhead,
    playing,
    cues,
    selected = null,
    mediaVersion = 0,
    onplayhead,
    onended,
    onmove,
    onzoom,
    player = $bindable(null),
  }: {
    comp: Composition;
    resolve: Resolve;
    playhead: number;
    playing: boolean;
    cues: Cue[];
    /** The layer of the selected clip, if it is on screen now. */
    selected?: Layer | null;
    /** Bumped when media became playable (a proxy, a file allowed). */
    mediaVersion?: number;
    onplayhead: (ms: number) => void;
    onended: () => void;
    /** The picture was dragged by this fraction of the frame. */
    onmove?: (dx: number, dy: number, done: boolean) => void;
    /** Scrolled on the picture: zoom by this factor. */
    onzoom?: (factor: number) => void;
    player?: Player | null;
  } = $props();

  const filterId = `klip-grade-${Math.random().toString(36).slice(2, 10)}`;
  let canvas = $state<HTMLCanvasElement | null>(null);
  let boxW = $state(0);
  let boxH = $state(0);
  let tick = $state(0);

  const aspect = $derived(comp.crop.width > 0 && comp.crop.height > 0 ? comp.crop.width / comp.crop.height : 16 / 9);
  const frame = $derived.by(() => {
    let w = boxW;
    let h = w / aspect;
    if (h > boxH) {
      h = boxH;
      w = h * aspect;
    }
    return { w: Math.max(0, w), h: Math.max(0, h), left: (boxW - w) / 2, top: (boxH - h) / 2 };
  });
  const matrix = $derived(svgColourMatrix(comp.colour));
  const graded = $derived(!isIdentityColour(comp.colour));

  function observe(el: HTMLDivElement) {
    const ro = new ResizeObserver(() => {
      boxW = el.clientWidth;
      boxH = el.clientHeight;
    });
    ro.observe(el);
    boxW = el.clientWidth;
    boxH = el.clientHeight;
    return { destroy: () => ro.disconnect() };
  }

  onMount(() => {
    if (!canvas) return;
    const p = new Player(canvas, resolve);
    p.onTime = (ms) => onplayhead(ms);
    p.onEnded = () => onended();
    p.onDraw = () => (tick += 1);
    player = p;
  });

  onDestroy(() => {
    player?.destroy();
    player = null;
  });

  $effect(() => {
    player?.setComposition(comp);
  });
  $effect(() => {
    player?.setGrade(graded ? filterId : null);
  });
  $effect(() => {
    player?.setCues(cues);
  });
  $effect(() => {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.min(comp.crop.width || 1920, Math.round(frame.w * dpr));
    const h = Math.round(w / aspect);
    if (w > 1 && h > 1) player?.resize(w, h);
  });
  $effect(() => {
    void mediaVersion;
    player?.reload();
  });
  $effect(() => {
    const p = player;
    if (!p) return;
    if (playing) p.play();
    else p.pause();
  });
  // The playhead moved from outside (the timeline, a key): follow it.
  $effect(() => {
    const p = player;
    const t = playhead;
    if (!p) return;
    if (!p.isPlaying && p.time !== t) p.seek(t);
    else if (p.isPlaying && Math.abs(p.time - t) > 250) p.seek(t);
  });

  // The selected clip's frame, in CSS pixels over the canvas.
  const box = $derived.by(() => {
    void tick;
    if (!selected || !player || !canvas || canvas.width === 0) return null;
    const pl = player.placementOf(selected);
    if (!pl) return null;
    const k = frame.w / canvas.width;
    return { left: pl.x * k, top: pl.y * k, width: pl.width * k, height: pl.height * k };
  });

  let drag: { x: number; y: number } | null = null;
  function down(e: PointerEvent) {
    if (!onmove) return;
    drag = { x: e.clientX, y: e.clientY };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function move(e: PointerEvent) {
    if (!drag || frame.w <= 0) return;
    onmove?.((e.clientX - drag.x) / frame.w, (e.clientY - drag.y) / frame.h, false);
  }
  function up(e: PointerEvent) {
    if (!drag) return;
    onmove?.((e.clientX - drag.x) / frame.w, (e.clientY - drag.y) / frame.h, true);
    drag = null;
  }
  function wheel(e: WheelEvent) {
    if (!onzoom) return;
    e.preventDefault();
    onzoom(e.deltaY < 0 ? 1.05 : 1 / 1.05);
  }
</script>

<div class="viewer" use:observe>
  <svg class="defs" aria-hidden="true" width="0" height="0">
    <filter id={filterId} color-interpolation-filters="sRGB" x="0" y="0" width="100%" height="100%">
      <feColorMatrix type="matrix" values={matrix} />
    </filter>
  </svg>
  <div
    class="frame"
    style:width="{frame.w}px"
    style:height="{frame.h}px"
    style:left="{frame.left}px"
    style:top="{frame.top}px"
  >
    <canvas bind:this={canvas} aria-label="Preview"></canvas>
    {#if box && !playing}
      <div
        class="sel"
        role="slider"
        tabindex="-1"
        aria-label="Move the picture (drag) or zoom (scroll)"
        aria-valuenow={0}
        style:left="{box.left}px"
        style:top="{box.top}px"
        style:width="{box.width}px"
        style:height="{box.height}px"
        onpointerdown={down}
        onpointermove={move}
        onpointerup={up}
        onpointercancel={up}
        onwheel={wheel}
      ></div>
    {/if}
  </div>
</div>

<style>
  .viewer {
    position: relative;
    flex: 1;
    min-height: 0;
    background: var(--viewer);
    overflow: hidden;
  }
  .defs {
    position: absolute;
    width: 0;
    height: 0;
  }
  .frame {
    position: absolute;
    overflow: hidden;
    background: #000;
  }
  canvas {
    display: block;
    width: 100%;
    height: 100%;
  }
  .sel {
    position: absolute;
    border: 1px solid var(--accent);
    box-shadow: 0 0 0 1px rgb(0 0 0 / 50%);
    cursor: move;
    touch-action: none;
  }
</style>
