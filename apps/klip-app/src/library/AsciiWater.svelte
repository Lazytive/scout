<!-- The Library's animated ASCII water background (lib/ascii-water.ts): fills its
     positioned parent, and the pointer over that parent stirs it. -->
<script lang="ts">
  import { onMount } from 'svelte';

  import { startAsciiWater } from '../lib/ascii-water';

  let canvas: HTMLCanvasElement | undefined = $state();

  onMount(() => {
    const host = canvas?.parentElement;
    if (!canvas || !host) return;
    const water = startAsciiWater(canvas, host);
    return () => water?.stop();
  });
</script>

<canvas bind:this={canvas} aria-hidden="true"></canvas>

<style>
  canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    display: block;
    pointer-events: none;
  }
</style>
