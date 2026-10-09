<!-- Microphone level from `mic_level` events: RMS bar with a peak tick, -60…0 dBFS. -->
<script lang="ts">
  import { app } from '../store.svelte';

  const pos = (db: number) => Math.max(0, Math.min(1, (db + 60) / 60));
  const rms = $derived(pos(app.mic.rms));
  const peak = $derived(pos(app.mic.peak));
  const hot = $derived(app.mic.peak > -3);
</script>

<div class="meter" role="meter" aria-label="Microphone level" aria-valuemin={-60} aria-valuemax={0} aria-valuenow={Math.round(app.mic.rms)}>
  <div class="bar" class:hot style:width="{rms * 100}%"></div>
  <div class="peak" style:left="{peak * 100}%"></div>
</div>
<span class="db mono">{app.mic.peak <= -99 ? '—' : `${app.mic.peak.toFixed(0)} dB`}</span>

<style>
  .meter {
    position: relative;
    width: 260px;
    height: 8px;
    background: var(--panel-raised);
    border-radius: 4px;
    overflow: hidden;
  }
  .bar {
    position: absolute;
    inset: 0 auto 0 0;
    background: var(--good);
    transition: width 60ms linear;
  }
  .bar.hot {
    background: var(--warn);
  }
  .peak {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    background: var(--text-strong);
  }
  .db {
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
</style>
