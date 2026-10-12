<!--
  A clip's waveform (light blue, as in FilmCraft): the clip is `w` px wide and shows source
  `from`…`to` ms; only its visible part `a`…`b` px is drawn, two px per column, on a
  loudness (dB) scale so quiet stretches still show.
-->
<script lang="ts">
  import { loadPeaks, mergePeaks, peaksPath } from '../../lib/peaks';

  /** `urls`: the sound's files (a clip's tracks are drawn together). */
  let { urls, from, to, w, a, b }: { urls: string[]; from: number; to: number; w: number; a: number; b: number } =
    $props();

  let peaks = $state<Float32Array | null>(null);
  $effect(() => {
    const list = urls;
    peaks = null;
    if (list.length === 0) return;
    let live = true;
    void Promise.all(list.map((u) => loadPeaks(u))).then((all) => {
      if (live) peaks = mergePeaks(all.filter((p): p is Float32Array => p !== null));
    });
    return () => {
      live = false;
    };
  });
  const n = $derived(Math.max(1, Math.min(2000, Math.round((b - a) / 2))));
  const span = $derived(Math.max(1, w));
  const d = $derived(
    peaks && b > a ? peaksPath(peaks, from + (a / span) * (to - from), from + (b / span) * (to - from), n) : '',
  );
</script>

{#if d}
  <svg
    class="wave"
    style:left="{a}px"
    style:width="{b - a}px"
    viewBox="0 0 {n} 1"
    preserveAspectRatio="none"
    aria-hidden="true"><path {d} /></svg
  >
{/if}

<style>
  .wave {
    position: absolute;
    top: 0;
    height: 100%;
    pointer-events: none;
  }
  path {
    fill: #79c0f9;
  }
</style>
