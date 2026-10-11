<!--
  FilmCraft's audio meters beside the timeline: the preview's output, left and right, in
  dBFS (peak, falling 24 dB/s, with a 1.5 s peak hold). Reads the sound graph's
  analysers once a frame while the preview plays.
-->
<script lang="ts">
  import { ed } from '../../lib/editor.svelte';
  import type { Player } from './player';

  let { player }: { player: Player | null } = $props();

  const FLOOR = -60;
  const TICKS = [0, -6, -12, -18, -24, -30, -36, -42, -48, -54];
  let level = $state<[number, number]>([FLOOR, FLOOR]);
  let hold = $state<[number, number]>([FLOOR, FLOOR]);
  const holdUntil: [number, number] = [0, 0];

  const toDb = (v: number) => (v > 0 ? Math.max(FLOOR, 20 * Math.log10(v)) : FLOOR);
  const pct = (db: number) => ((Math.min(0, db) - FLOOR) / -FLOOR) * 100;

  $effect(() => {
    let raf = 0;
    let last = performance.now();
    const frame = (now: number) => {
      const dt = Math.min(0.1, (now - last) / 1000);
      last = now;
      const raw = ed.playing && player ? player.sound.levels() : ([0, 0] as [number, number]);
      const next: [number, number] = [FLOOR, FLOOR];
      const held: [number, number] = [hold[0], hold[1]];
      for (const i of [0, 1] as const) {
        const db = toDb(raw[i]);
        next[i] = Math.max(db, level[i] - 24 * dt);
        if (db >= held[i] || now > holdUntil[i]) {
          held[i] = db;
          holdUntil[i] = now + 1500;
        }
      }
      if (next[0] !== level[0] || next[1] !== level[1]) level = next;
      if (held[0] !== hold[0] || held[1] !== hold[1]) hold = held;
      raf = requestAnimationFrame(frame);
    };
    raf = requestAnimationFrame(frame);
    return () => cancelAnimationFrame(raf);
  });
</script>

<div class="meters" role="meter" aria-label="Output level" aria-valuemin={FLOOR} aria-valuemax={0} aria-valuenow={Math.round(Math.max(level[0], level[1]))}>
  <div class="bars">
    {#each [0, 1] as i (i)}
      <div class="bar">
        <div class="fill" style:clip-path="inset({100 - pct(level[i] ?? FLOOR)}% 0 0 0)"></div>
        {#if (hold[i] ?? FLOOR) > FLOOR}<div class="hold" class:clip={(hold[i] ?? FLOOR) >= -0.1} style:bottom="{pct(hold[i] ?? FLOOR)}%"></div>{/if}
      </div>
    {/each}
    <div class="scale mono">
      {#each TICKS as t (t)}
        <span style:bottom="{pct(t)}%">{t}</span>
      {/each}
    </div>
  </div>
  <div class="labels mono"><span>L</span><span>R</span><span class="db">dB</span></div>
</div>

<style>
  .meters {
    display: flex;
    flex-direction: column;
    height: 100%;
    padding: 8px 6px 4px;
    background: var(--panel);
    min-width: 0;
  }
  .bars {
    position: relative;
    flex: 1;
    display: flex;
    gap: 3px;
    min-height: 0;
  }
  .bar {
    position: relative;
    width: 12px;
    background: var(--viewer);
    border: 1px solid var(--border);
  }
  /* The colours sit at fixed levels: −12 dB turns yellow, −6 orange, 0 red. */
  .fill {
    position: absolute;
    inset: 0;
    background: linear-gradient(to top, #2f9e4f 0%, #4fc35f 78%, #d9c440 82%, #e8913a 92%, #f0555b 100%);
  }
  .hold {
    position: absolute;
    left: 0;
    right: 0;
    height: 2px;
    background: #d9c440;
  }
  .hold.clip {
    background: var(--bad);
  }
  .scale {
    position: relative;
    flex: 1;
    min-width: 22px;
  }
  .scale span {
    position: absolute;
    right: 0;
    transform: translateY(50%);
    font-size: calc(9px * var(--text-scale));
    color: var(--text-muted);
  }
  .labels {
    display: flex;
    gap: 3px;
    padding-top: 3px;
    font-size: calc(9px * var(--text-scale));
    color: var(--text-muted);
  }
  .labels span {
    width: 14px;
    text-align: center;
  }
  .labels .db {
    flex: 1;
    text-align: right;
  }
  .mono {
    font-family: var(--font-mono);
  }
</style>
