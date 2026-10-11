<!-- A sound's waveform between two source times, stretched over its box (light blue, as in FilmCraft). -->
<script lang="ts">
  import { loadPeaks, peaksPath } from '../../lib/peaks';

  let { url, from, to, buckets }: { url: string | null; from: number; to: number; buckets: number } = $props();

  let peaks = $state<Float32Array | null>(null);
  $effect(() => {
    const u = url;
    peaks = null;
    if (!u) return;
    let live = true;
    void loadPeaks(u).then((p) => {
      if (live) peaks = p;
    });
    return () => {
      live = false;
    };
  });
  const n = $derived(Math.max(1, Math.min(1200, Math.round(buckets))));
  const d = $derived(peaks ? peaksPath(peaks, from, to, n) : '');
</script>

{#if d}
  <svg class="wave" viewBox="0 0 {n} 1" preserveAspectRatio="none" aria-hidden="true"><path {d} /></svg>
{/if}

<style>
  .wave {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }
  path {
    fill: #79c0f9;
  }
</style>
