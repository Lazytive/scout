<!--
  Effect Controls, FilmCraft's (Premiere's) way: the selected clip's Motion (zoom and
  position share keys), Opacity, Volume and Speed, each with a stopwatch (animate on/off),
  blue drag-to-change values, ◀ ◆ ▶ (previous key / add or remove a key at the playhead /
  next key) and a key lane on the right showing just this clip: drag a diamond to move
  its key, click it to go there, double-click it to remove it; click the lane to move
  the playhead. Values change the clip's constant while the stopwatch is off and key the
  playhead while it is on.
-->
<script lang="ts">
  import type { Lane, PlacedClip, Track } from '../../lib/advanced';
  import {
    LANES,
    RATE_MAX,
    RATE_MIN,
    clipLength,
    laneValue,
    moveKey,
    overlapOf,
    rateAt,
    readAdvanced,
    removeSpeedPoint,
    setSpeedPoint,
    sourceLabel,
  } from '../../lib/advanced';
  import type { Field, Group } from '../../lib/effect-controls';
  import {
    applyValue,
    groupTimes,
    isAnimated,
    nextKey,
    prevKey,
    resetGroup,
    spreadKeys,
    startAnimating,
    stopAnimating,
    toggleKey,
  } from '../../lib/effect-controls';
  import { beginGesture, ed, edit, endGesture, gesture, liveEdit, seek } from '../../lib/editor.svelte';
  import { formatTimecode } from '../../lib/format';
  import type { Project } from '../../lib/timeline';
  import { pastThreshold } from '../../lib/timeline-view';
  import HotValue from './HotValue.svelte';

  let { project, mainName, names }: { project: Project; mainName: string; names: Map<string, string> } = $props();

  const adv = $derived(readAdvanced(project));
  const found = $derived.by(() => {
    for (const t of adv.tracks) {
      const c = t.clips.find((x) => x.id === ed.clipId);
      if (c) return { track: t, clip: c };
    }
    return null;
  });

  let laneW = $state(200);
  const DIAMOND = 11;

  /** Where the clip starts playing (a crossfade pulls it under the one before). */
  const startOf = (t: Track, c: PlacedClip) => c.at_ms - overlapOf(adv, t, c);

  interface Spec {
    label: string;
    scale: number;
    decimals: number;
    unit: string;
    min: number;
    max: number;
    step: number;
  }
  function spec(f: Field): Spec {
    if (f === 'speed')
      return { label: 'Speed', scale: 1, decimals: 0, unit: '%', min: RATE_MIN, max: RATE_MAX, step: 1 };
    const l = LANES.find((x) => x.id === f)!;
    const label =
      f === 'zoom'
        ? 'Zoom'
        : f === 'x'
          ? 'Position X'
          : f === 'y'
            ? 'Position Y'
            : f === 'opacity'
              ? 'Opacity'
              : 'Level';
    return {
      label,
      scale: 100,
      decimals: f === 'volume' ? 0 : 1,
      unit: '%',
      min: l.min * 100,
      max: l.max * 100,
      step: f === 'volume' ? 1 : 0.5,
    };
  }
  const valueOf = (c: PlacedClip, f: Field, t: number) =>
    f === 'speed' ? rateAt(c, t) : laneValue(c, f as Lane, t) * 100;

  const GROUP_LABEL: Record<Group, string> = { motion: 'Motion', opacity: 'Opacity', volume: 'Volume', speed: 'Speed' };
  function groupsFor(t: Track, c: PlacedClip): { group: Group; fields: Field[] }[] {
    const image = c.source.kind === 'image';
    if (t.kind === 'audio' || t.kind === 'music')
      return [
        { group: 'volume', fields: ['volume'] },
        { group: 'speed', fields: ['speed'] },
      ];
    const out: { group: Group; fields: Field[] }[] = [
      { group: 'motion', fields: ['zoom', 'x', 'y'] },
      { group: 'opacity', fields: ['opacity'] },
    ];
    if (!image) out.push({ group: 'volume', fields: ['volume'] }, { group: 'speed', fields: ['speed'] });
    return out;
  }

  const stickyKey = (c: PlacedClip, g: Group) => `${c.id}:${g}`;
  const animated = (c: PlacedClip, g: Group) => isAnimated(c, g, ed.animating[stickyKey(c, g)] === true);

  function set(c: PlacedClip, f: Field, at: number, shown: number, live: boolean) {
    const g = f === 'zoom' || f === 'x' || f === 'y' ? 'motion' : f;
    const v = f === 'speed' ? shown : shown / 100;
    const anim = animated(c, g);
    const label = `Set ${spec(f).label.toLowerCase()}`;
    if (live) liveEdit((p) => applyValue(p, c.id, f, at, v, anim), label);
    else edit((p) => applyValue(p, c.id, f, at, v, anim), label);
  }

  function stopwatch(c: PlacedClip, g: Group, at: number) {
    const k = stickyKey(c, g);
    if (animated(c, g)) {
      edit((p) => stopAnimating(p, c.id, g, at), `Stop animating ${GROUP_LABEL[g].toLowerCase()}`);
      delete ed.animating[k];
    } else {
      edit((p) => startAnimating(p, c.id, g, at), `Animate ${GROUP_LABEL[g].toLowerCase()}`);
      ed.animating[k] = true;
    }
  }

  function toggle(c: PlacedClip, g: Group, at: number) {
    const has = groupTimes(c, g).includes(at);
    if (edit((p) => toggleKey(p, c.id, g, at), has ? 'Remove a key' : 'Add a key'))
      ed.animating[stickyKey(c, g)] = true;
  }

  // ---- the key lane ----
  function laneSeek(e: PointerEvent, t: Track, c: PlacedClip) {
    if (e.button !== 0) return;
    const el = e.currentTarget as HTMLElement;
    const go = (x: number) => {
      const r = el.getBoundingClientRect();
      const f = Math.min(1, Math.max(0, (x - r.left) / Math.max(1, r.width)));
      ed.playing = false;
      seek(startOf(t, c) + Math.round(f * clipLength(c)));
    };
    go(e.clientX);
    const move = (m: PointerEvent) => go(m.clientX);
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  }

  function keyDown(e: PointerEvent, t: Track, c: PlacedClip, g: Group, from: number) {
    if (e.button !== 0) return;
    e.stopPropagation();
    const x0 = e.clientX;
    const y0 = e.clientY;
    const len = clipLength(c);
    let started = false;
    const move = (m: PointerEvent) => {
      if (!started) {
        if (!pastThreshold(m.clientX - x0, m.clientY - y0)) return;
        started = true;
        beginGesture();
      }
      const to = Math.min(len, Math.max(0, Math.round(from + ((m.clientX - x0) / Math.max(1, laneW)) * len)));
      if (g === 'speed') {
        if (from === 0) return;
        const rate = rateAt(c, from);
        gesture((p) => {
          removeSpeedPoint(p, c.id, from);
          setSpeedPoint(p, c.id, to, rate);
        });
      } else gesture((p) => moveKey(p, c.id, g === 'motion' ? 'zoom' : g, from, to));
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', up);
      if (started) endGesture(g === 'speed' ? 'Move a speed point' : 'Move a key');
      else {
        ed.playing = false;
        seek(startOf(t, c) + from);
      }
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', up);
  }

  /** The diamonds of a group: key times and where to draw them (spread when crowded). */
  function diamonds(c: PlacedClip, g: Group): { t: number; x: number }[] {
    const times = groupTimes(c, g);
    const len = Math.max(1, clipLength(c));
    const xs = spreadKeys(
      times.map((t) => (t / len) * laneW),
      DIAMOND - 1,
      laneW,
    );
    return times.map((t, i) => ({ t, x: xs[i] ?? 0 }));
  }
</script>

{#if found}
  {@const t = found.track}
  {@const c = found.clip}
  {@const from = startOf(t, c)}
  {@const len = clipLength(c)}
  {@const at = Math.min(len, Math.max(0, ed.playhead - from))}
  {@const inside = ed.playhead >= from && ed.playhead <= from + len}
  {@const headX = (at / Math.max(1, len)) * 100}
  <div class="ec">
    <div class="row top">
      <div class="props">
        <span class="pill" title={sourceLabel(c.source, mainName, names)}
          >{t.name || 'Track'} · {sourceLabel(c.source, mainName, names)}</span
        >
      </div>
      <div class="lane head" bind:clientWidth={laneW} role="presentation" onpointerdown={(e) => laneSeek(e, t, c)}>
        <span class="clipbar">{sourceLabel(c.source, mainName, names)}</span>
        <span class="tick start mono">{formatTimecode(0)}</span>
        <span class="tick end mono">{formatTimecode(len)}</span>
        {#if inside}<span class="head-mark" style:left="{headX}%"></span>{/if}
      </div>
    </div>

    {#each groupsFor(t, c) as grp (grp.group)}
      {@const g = grp.group}
      {@const anim = animated(c, g)}
      {@const times = groupTimes(c, g)}
      {@const onKey = times.includes(at)}
      {@const single = grp.fields.length === 1}
      {#snippet controls()}
        <button
          class="watch"
          class:on={anim}
          aria-pressed={anim}
          title={anim ? 'Stop animating (keeps the value at the playhead)' : 'Animate: keys from now on'}
          onclick={() => stopwatch(c, g, at)}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true"
            ><circle cx="8" cy="9" r="5.5" /><path d="M8 9V6.2M6.5 2h3M12.3 4.3l1 -1" /></svg
          >
        </button>
      {/snippet}
      {#snippet nav()}
        <span class="nav">
          <button
            class="flat tiny"
            disabled={!anim || prevKey(times, at) === null}
            title="Previous key"
            onclick={() => seek(from + (prevKey(times, at) ?? 0))}>◀</button
          >
          <button
            class="flat tiny key"
            class:on={onKey}
            aria-pressed={onKey}
            title={onKey ? 'Remove the key here' : 'Add a key here'}
            onclick={() => toggle(c, g, at)}>◆</button
          >
          <button
            class="flat tiny"
            disabled={!anim || nextKey(times, at) === null}
            title="Next key"
            onclick={() => seek(from + (nextKey(times, at) ?? 0))}>▶</button
          >
        </span>
      {/snippet}
      {#snippet laneCell(withKeys: boolean)}
        <div class="lane" role="presentation" onpointerdown={(e) => laneSeek(e, t, c)}>
          {#if withKeys}
            {#each diamonds(c, g) as d (d.t)}
              <span
                class="dia"
                class:speed={g === 'speed'}
                class:here={d.t === at}
                style:left="{d.x}px"
                role="button"
                tabindex="-1"
                title="{formatTimecode(d.t)}{g === 'speed'
                  ? ` · ${rateAt(c, d.t)} %`
                  : ''} (drag to move, double-click to remove)"
                onpointerdown={(e) => keyDown(e, t, c, g, d.t)}
                ondblclick={(e) => {
                  e.stopPropagation();
                  edit((p) => toggleKey(p, c.id, g, d.t), 'Remove a key');
                }}
                onkeydown={() => {}}
              ></span>
            {/each}
          {/if}
          {#if inside}<span class="line" style:left="{headX}%"></span>{/if}
        </div>
      {/snippet}

      <div class="row group">
        <div class="props">
          <span class="fx">fx</span>
          <span class="gname">{GROUP_LABEL[g]}</span>
          {#if !single}
            {@render controls()}
            {@render nav()}
          {/if}
          <span class="grow"></span>
          <button
            class="flat tiny reset"
            title="Reset {GROUP_LABEL[g].toLowerCase()}"
            onclick={() => {
              edit((p) => resetGroup(p, c.id, g), `Reset ${GROUP_LABEL[g].toLowerCase()}`);
              delete ed.animating[stickyKey(c, g)];
            }}>↺</button
          >
        </div>
        {@render laneCell(!single)}
      </div>
      {#each grp.fields as f (f)}
        {@const s = spec(f)}
        <div class="row prop">
          <div class="props">
            {#if single}{@render controls()}{:else}<span class="watch-space"></span>{/if}
            <span class="pname">{s.label}</span>
            <HotValue
              value={Number(valueOf(c, f, at).toFixed(s.decimals))}
              min={s.min}
              max={s.max}
              step={s.step}
              decimals={s.decimals}
              unit={s.unit}
              label={s.label}
              onlive={(v) => set(c, f, at, v, true)}
              onset={(v) => set(c, f, at, v, false)}
            />
            <span class="grow"></span>
            {#if single}{@render nav()}{/if}
          </div>
          {@render laneCell(single)}
        </div>
      {/each}
      {#if g === 'speed'}
        <p class="hint muted">
          Speed changes from each point on; the clip keeps its part of the source, so slower makes it longer. The pitch
          stays.
        </p>
      {/if}
    {/each}

    <div class="foot">
      <span class="tc mono" title="The playhead in this clip">{formatTimecode(at)}</span>
      {#if !inside}<span class="muted">The playhead is outside this clip.</span>{/if}
    </div>
  </div>
{:else}
  <p class="muted hint">
    Select a clip on the timeline to see its effects here: zoom and position, opacity, volume and speed, with keys.
  </p>
{/if}

<style>
  .ec {
    display: flex;
    flex-direction: column;
    font-size: var(--text-sm);
    min-width: 0;
  }
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(90px, 42%);
    min-height: 24px;
  }
  .props {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 6px;
    min-width: 0;
  }
  .top .props {
    padding: 4px 6px;
  }
  .pill {
    max-width: 100%;
    padding: 2px 8px;
    border-radius: 999px;
    border: 1px solid var(--border-strong);
    background: var(--panel-raised);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    font-size: var(--text-xs);
  }
  .lane {
    position: relative;
    border-left: 1px solid var(--border);
    background: rgb(0 0 0 / 12%);
    cursor: text;
    touch-action: none;
  }
  .lane.head {
    height: 34px;
    background: var(--panel);
  }
  .clipbar {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 2px;
    height: 14px;
    padding: 0 4px;
    background: var(--panel-raised);
    border: 1px solid var(--border-strong);
    font-size: calc(10px * var(--text-scale));
    line-height: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    pointer-events: none;
  }
  .tick {
    position: absolute;
    top: 1px;
    font-size: calc(10px * var(--text-scale));
    color: var(--text-muted);
    pointer-events: none;
  }
  .tick.start {
    left: 3px;
  }
  .tick.end {
    right: 3px;
  }
  .head-mark {
    position: absolute;
    top: 0;
    width: 9px;
    height: 12px;
    margin-left: -4.5px;
    background: var(--accent-text);
    clip-path: polygon(0 0, 100% 0, 100% 65%, 50% 100%, 0 65%);
    pointer-events: none;
  }
  .line {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: var(--accent-text);
    pointer-events: none;
  }
  .group {
    border-top: 1px solid var(--border);
  }
  .fx {
    font-style: italic;
    font-weight: 600;
    color: var(--text-muted);
    font-size: var(--text-xs);
  }
  .gname {
    color: var(--text-strong);
  }
  .pname {
    min-width: 72px;
    color: var(--text);
  }
  .watch,
  .watch-space {
    flex: none;
    width: 20px;
    height: 20px;
  }
  .watch {
    min-width: 0;
    padding: 0;
    display: grid;
    place-items: center;
    background: transparent;
    border: none;
    color: var(--text-muted);
  }
  .watch.on {
    color: var(--accent-text);
  }
  svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
    stroke-linecap: round;
  }
  .nav {
    display: inline-flex;
    gap: 1px;
  }
  .tiny {
    min-width: 0;
    height: 20px;
    padding: 0 3px;
    font-size: calc(10px * var(--text-scale));
  }
  .key.on {
    color: var(--accent-text);
  }
  .reset {
    color: var(--text-muted);
  }
  .dia {
    position: absolute;
    top: 50%;
    width: 9px;
    height: 9px;
    margin: -5px 0 0 -5px;
    transform: rotate(45deg);
    background: var(--panel-raised);
    border: 1px solid var(--text-strong);
    cursor: ew-resize;
    touch-action: none;
  }
  .dia.here {
    background: var(--accent-text);
  }
  .dia.speed {
    transform: none;
    border-radius: 2px;
  }
  .grow {
    flex: 1;
  }
  .hint {
    margin: 2px 6px 6px;
    font-size: var(--text-xs);
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px;
    border-top: 1px solid var(--border);
  }
  .tc {
    color: var(--accent-text);
  }
  .mono {
    font-family: var(--font-mono);
  }
  .muted {
    color: var(--text-muted);
  }
</style>
