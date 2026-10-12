<!--
  The editor's preview: the clip (or its proxy) in a <video>, played along the OUTPUT
  timeline (segments seeked in order at their speed), the 9:16 crop window to drag, and
  the text cues as the engine renders them. The sound is the clip's tracks following
  the video through Web Audio (`FollowSound`), so the Game/Mic sliders are heard. Without
  playable media (the mock, an unplayable codec) the strip picture of the current frame
  stands in.
-->
<script lang="ts">
  import { onDestroy } from 'svelte';

  import { cropRect, msToPct, tileFor } from '../lib/editor';
  import type { SourceInfo } from '../lib/editor';
  import { formatTimecode } from '../lib/format';
  import { stripPosition } from '../lib/library';
  import type { Project } from '../lib/timeline';
  import { outputDurationMs, outputTime, segments, sourceTime } from '../lib/timeline';
  import { FollowSound } from './advanced/sound';

  export interface Overlay {
    id: number;
    url: string;
    x: number;
    y: number;
    width: number;
    height: number;
    frame_width: number;
    frame_height: number;
    start_ms: number;
    end_ms: number;
  }

  let {
    src,
    strip,
    tiles,
    source,
    project,
    playhead,
    playing,
    overlays,
    tracks = [],
    onplayhead,
    onended,
    oncrop,
  }: {
    src: string | null;
    strip: string | null;
    tiles: number;
    source: SourceInfo;
    project: Project;
    playhead: number;
    playing: boolean;
    overlays: Overlay[];
    /** The clip's sound tracks, one playable file each (empty: the video's own sound). */
    tracks?: { name: string; url: string }[];
    onplayhead: (outMs: number) => void;
    onended: () => void;
    oncrop: (x: number) => void;
  } = $props();

  let video = $state<HTMLVideoElement | null>(null);
  let box = $state<HTMLDivElement | null>(null);
  let boxW = $state(0);
  let boxH = $state(0);
  let failed = $state(false);
  let segIndex = -1;

  const srcMs = $derived(sourceTime(project, playhead) ?? 0);
  const crop = $derived(cropRect(project.format.kind, project.format.crop_x, source.width, source.height));
  const tall = $derived(project.format.kind === 'tall');
  // The frame as drawn: fit the source aspect in the box.
  const frame = $derived.by(() => {
    const aspect = source.width > 0 && source.height > 0 ? source.width / source.height : 16 / 9;
    let w = boxW;
    let h = w / aspect;
    if (h > boxH) {
      h = boxH;
      w = h * aspect;
    }
    return { w, h, left: (boxW - w) / 2, top: (boxH - h) / 2 };
  });
  const scale = $derived(source.width > 0 ? frame.w / source.width : 0);
  const usable = $derived(src !== null && !failed);
  const visible = $derived(overlays.filter((o) => playhead >= o.start_ms && playhead < o.end_ms));

  // The sound: the tracks follow the video, weighed by the sliders. State, so the
  // tracks and volumes are handed to every new follower (the tracks may arrive first).
  let sound = $state.raw<FollowSound | null>(null);
  $effect(() => {
    const v = video;
    if (!v || !usable) return;
    // Not reading `sound` here: this effect writes it.
    const s = new FollowSound(v);
    sound = s;
    return () => {
      s.destroy();
      sound = null;
    };
  });
  $effect(() => {
    const list = tracks;
    if (sound && video) sound.setTracks(list);
  });
  $effect(() => {
    const vols = project.volumes;
    sound?.setVolumes(vols);
  });
  onDestroy(() => sound?.destroy());

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

  // Seek while paused (or when the playhead jumps).
  $effect(() => {
    const v = video;
    if (!v || !usable || playing) return;
    const t = srcMs / 1000;
    if (Math.abs(v.currentTime - t) > 0.02) v.currentTime = t;
  });

  // Play: run the segment under the playhead at its speed, hop to the next at its end.
  $effect(() => {
    const v = video;
    if (!v || !usable) return;
    if (!playing) {
      v.pause();
      segIndex = -1;
      return;
    }
    const segs = segments(project);
    const i = segs.findIndex((s) => playhead >= s.out_start_ms && playhead < s.out_end_ms);
    const seg = segs[i];
    if (!seg) {
      onended();
      return;
    }
    if (i !== segIndex) {
      segIndex = i;
      v.currentTime = (sourceTime(project, playhead) ?? seg.src_start_ms) / 1000;
      v.playbackRate = seg.speed;
      sound?.resume();
      void v.play().catch(() => (failed = true));
    }
  });

  function tick() {
    const v = video;
    if (!v || !playing) return;
    const segs = segments(project);
    const seg = segs[segIndex];
    if (!seg) return;
    const t = Math.round(v.currentTime * 1000);
    if (t >= seg.src_end_ms) {
      const next = segs[segIndex + 1];
      if (!next) {
        onplayhead(Math.max(0, outputDurationMs(project) - 1));
        onended();
        return;
      }
      segIndex += 1;
      v.currentTime = next.src_start_ms / 1000;
      v.playbackRate = next.speed;
      onplayhead(next.out_start_ms);
      return;
    }
    const out = outputTime(project, t);
    if (out !== null) onplayhead(out);
  }

  // Crop drag.
  let drag: { startX: number; startCrop: number; spare: number } | null = null;
  function cropDown(e: PointerEvent) {
    if (!tall) return;
    const spare = (source.width - crop.width) * scale;
    if (spare <= 0) return;
    drag = { startX: e.clientX, startCrop: project.format.crop_x, spare };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function cropMove(e: PointerEvent) {
    if (!drag) return;
    const x = drag.startCrop + (e.clientX - drag.startX) / drag.spare;
    oncrop(Math.min(1, Math.max(0, x)));
  }
  function cropUp() {
    drag = null;
  }
</script>

<div class="viewer" bind:this={box} use:observe>
  <div
    class="frame"
    style:width="{frame.w}px"
    style:height="{frame.h}px"
    style:left="{frame.left}px"
    style:top="{frame.top}px"
  >
    {#if usable}
      <!-- svelte-ignore a11y_media_has_caption -->
      <video
        bind:this={video}
        {src}
        crossorigin="anonymous"
        preload="auto"
        playsinline
        ontimeupdate={tick}
        onerror={() => (failed = true)}
        {onended}
      ></video>
    {:else if strip}
      <div
        class="still"
        style:background-image="url({strip})"
        style:background-size="100% {tiles * 100}%"
        style:background-position-y={stripPosition(tileFor(srcMs, source.duration_ms, tiles), tiles)}
        role="img"
        aria-label="Frame at {formatTimecode(srcMs)}"
      ></div>
    {:else}
      <div class="still empty"></div>
    {/if}
    {#each visible as o (o.id)}
      <img
        class="cue"
        src={o.url}
        alt=""
        draggable="false"
        style:left="{msToPct(crop.x + (o.x * crop.width) / o.frame_width, source.width)}%"
        style:top="{msToPct((o.y * crop.height) / o.frame_height, source.height)}%"
        style:width="{msToPct((o.width * crop.width) / o.frame_width, source.width)}%"
        style:height="{msToPct((o.height * crop.height) / o.frame_height, source.height)}%"
      />
    {/each}
    {#if tall}
      <div class="shade" style:left="0" style:width="{msToPct(crop.x, source.width)}%"></div>
      <div class="shade" style:left="{msToPct(crop.x + crop.width, source.width)}%" style:right="0"></div>
      <div
        class="window"
        role="slider"
        aria-label="9:16 crop position"
        aria-valuemin="0"
        aria-valuemax="100"
        aria-valuenow={Math.round(project.format.crop_x * 100)}
        tabindex="0"
        style:left="{msToPct(crop.x, source.width)}%"
        style:width="{msToPct(crop.width, source.width)}%"
        onpointerdown={cropDown}
        onpointermove={cropMove}
        onpointerup={cropUp}
        onpointercancel={cropUp}
        onkeydown={(e) => {
          if (e.key === 'ArrowLeft') oncrop(Math.max(0, project.format.crop_x - 0.02));
          if (e.key === 'ArrowRight') oncrop(Math.min(1, project.format.crop_x + 0.02));
        }}
      ></div>
    {/if}
  </div>
  {#if !usable && src !== null}
    <p class="note">This clip can’t be played here; the still frames stand in.</p>
  {/if}
</div>

<style>
  .viewer {
    position: relative;
    flex: 1;
    min-height: 0;
    background: var(--viewer);
    overflow: hidden;
  }
  .frame {
    position: absolute;
    overflow: hidden;
    background: #000;
  }
  video,
  .still {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    object-fit: contain;
    background-repeat: no-repeat;
  }
  .still.empty {
    background: var(--panel-raised);
  }
  .cue {
    position: absolute;
    pointer-events: none;
  }
  .shade {
    position: absolute;
    top: 0;
    bottom: 0;
    background: rgb(0 0 0 / 65%);
    pointer-events: none;
  }
  .window {
    position: absolute;
    top: 0;
    bottom: 0;
    border: 2px solid var(--accent);
    box-sizing: border-box;
    cursor: ew-resize;
    touch-action: none;
  }
  .window:focus-visible {
    outline: 2px solid var(--focus);
  }
  .note {
    position: absolute;
    left: var(--space-3);
    bottom: var(--space-2);
    margin: 0;
    font-size: var(--text-xs);
    color: var(--text-muted);
  }
</style>
