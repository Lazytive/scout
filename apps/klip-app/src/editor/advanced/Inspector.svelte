<!--
  The selected clip in numbers: where it sits, its zoom/position/opacity/volume at the
  playhead (◆ adds or removes a key there; without keys a value holds for the whole
  clip), its speed points, the transition into the next clip on its track, and its
  track (name, mute, order).
-->
<script lang="ts">
  import type { Lane, PlacedClip, Track, TransitionKind } from '../../lib/advanced';
  import {
    LANES,
    MAX_TRANSITION_MS,
    RATE_MAX,
    RATE_MIN,
    addKey,
    clearLane,
    clipLength,
    laneKeys,
    laneValue,
    moveClip,
    moveTrack,
    nextOnTrack,
    overlapOf,
    rateAt,
    readAdvanced,
    removeClip,
    removeKey,
    removeSpeedPoint,
    removeTrack,
    setLane,
    setSpeedPoint,
    setTrack,
    setTransition,
    sourceLabel,
    splitClip,
    transitionAfter,
  } from '../../lib/advanced';
  import { ed, edit, seek } from '../../lib/editor.svelte';
  import NumberField from '../../lib/forms/NumberField.svelte';
  import Segmented from '../../lib/forms/Segmented.svelte';
  import { formatTimecode } from '../../lib/format';
  import type { Project } from '../../lib/timeline';

  let { project, mainName, names }: { project: Project; mainName: string; names: Map<string, string> } = $props();

  const adv = $derived(readAdvanced(project));
  const found = $derived.by(() => {
    for (const t of adv.tracks) {
      const c = t.clips.find((x) => x.id === ed.clipId);
      if (c) return { track: t, clip: c };
    }
    return null;
  });

  /** Where the clip starts playing (a crossfade pulls it under the one before). */
  function start(t: Track, c: PlacedClip): number {
    return c.at_ms - overlapOf(adv, t, c);
  }

  /** The playhead in the clip's own time (what its keys are in). */
  function local(t: Track, c: PlacedClip): number {
    return Math.min(clipLength(c), Math.max(0, ed.playhead - start(t, c)));
  }

  function lanesFor(t: Track, c: PlacedClip): typeof LANES {
    if (c.source.kind === 'image') return LANES.filter((l) => l.id !== 'volume');
    if (t.kind === 'audio' || t.kind === 'music') return LANES.filter((l) => l.id === 'volume');
    return LANES;
  }

  const fmt = (lane: Lane, v: number) => (lane === 'opacity' || lane === 'volume' ? Math.round(v * 100) : Math.round(v * 1000) / 1000);
  const unfmt = (lane: Lane, v: number) => (lane === 'opacity' || lane === 'volume' ? v / 100 : v);

  /** Every time (clip ms) that holds any key, sorted. */
  function keys(c: PlacedClip): number[] {
    const all = [...c.transform, ...c.opacity, ...c.volume].map((k) => k.at_ms).sort((a, b) => a - b);
    return all.filter((t, i) => i === 0 || all[i - 1] !== t);
  }

  const TRANSITIONS: { value: TransitionKind; label: string }[] = [
    { value: 'cut', label: 'Cut' },
    { value: 'crossfade', label: 'Crossfade' },
    { value: 'dip', label: 'Dip' },
  ];
</script>

{#if found}
  {@const t = found.track}
  {@const c = found.clip}
  {@const at = local(t, c)}
  {@const from = start(t, c)}
  {@const next = nextOnTrack(adv, c.id)}
  {@const tr = transitionAfter(adv, t.id, c.id)}
  <section>
    <h3>{sourceLabel(c.source, mainName, names)}</h3>
    <p class="hint muted mono">
      {formatTimecode(from)} – {formatTimecode(from + clipLength(c))} · {t.name || 'Track'}
    </p>
    <div class="row">
      <button title="Split at the playhead (S)" onclick={() => edit((p) => void splitClip(p, c.id, c.at_ms + at), 'Split clip')}>Split</button>
      <button class="danger" title="Remove (Delete)" onclick={() => {
        edit((p) => removeClip(p, c.id), 'Remove clip');
        ed.clipId = null;
      }}>Remove</button>
    </div>
    <div class="grid">
      <span>Starts at</span>
      <NumberField label="Starts at" value={c.at_ms / 1000} min={0} max={86_400} step={0.001} unit="s" onchange={(v) => edit((p) => moveClip(p, c.id, v * 1000), 'Move clip')} />
    </div>
  </section>

  <section>
    <h3>At the playhead <span class="muted mono">{formatTimecode(at)}</span></h3>
    {#each lanesFor(t, c) as l (l.id)}
      {@const keys = laneKeys(c, l.id)}
      {@const onKey = keys.includes(at)}
      <div class="lane">
        <span class="name">{l.label}</span>
        <NumberField
          label={l.label}
          value={fmt(l.id, laneValue(c, l.id, at))}
          min={fmt(l.id, l.min)}
          max={fmt(l.id, l.max)}
          step={l.id === 'opacity' || l.id === 'volume' ? 1 : l.step}
          unit={l.id === 'opacity' || l.id === 'volume' ? '%' : ''}
          onchange={(v) => edit((p) => setLane(p, c.id, l.id, at, unfmt(l.id, v)), `Set ${l.label.toLowerCase()}`)}
        />
        <button
          class="flat key"
          class:on={onKey}
          aria-pressed={onKey}
          title={onKey ? 'Remove the key here' : 'Add a key here'}
          onclick={() => edit((p) => (onKey ? removeKey(p, c.id, l.id, at) : addKey(p, c.id, l.id, at)), onKey ? 'Remove a key' : 'Add a key')}
        >◆</button>
        {#if keys.length > 0}
          <button class="flat tiny" title="Clear: no keys, the neutral value" onclick={() => edit((p) => clearLane(p, c.id, l.id), `Clear ${l.label.toLowerCase()}`)}>×</button>
        {/if}
      </div>
    {/each}
    {#if keys(c).length > 0}
      <div class="row wrap">
        {#each keys(c) as k (k)}
          <button class="flat tiny mono" title="Go to this key" onclick={() => seek(from + k)}>{formatTimecode(k)}</button>
        {/each}
      </div>
    {/if}
  </section>

  {#if c.source.kind !== 'image'}
    <section>
      <h3>Speed</h3>
      <div class="lane">
        <span class="name">From here</span>
        <NumberField label="Speed" value={rateAt(c, at)} min={RATE_MIN} max={RATE_MAX} step={5} unit="%" onchange={(v) => edit((p) => setSpeedPoint(p, c.id, at, v), 'Set speed')} />
      </div>
      {#if c.speed.length > 0}
        <ul class="points">
          {#each c.speed as s (s.at_ms)}
            <li>
              <button class="flat tiny mono" onclick={() => seek(from + s.at_ms)}>{formatTimecode(s.at_ms)}</button>
              <span class="mono">{s.rate_pct} %</span>
              {#if s.at_ms > 0}
                <button class="flat tiny" title="Remove" onclick={() => edit((p) => removeSpeedPoint(p, c.id, s.at_ms), 'Remove a speed point')}>×</button>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      <p class="hint muted">The clip keeps its part of the source: slower makes it longer. The pitch stays.</p>
    </section>
  {/if}

  {#if next && t.kind === 'video'}
    <section>
      <h3>Into the next clip</h3>
      <Segmented
        label="Transition"
        value={tr?.kind ?? 'cut'}
        options={TRANSITIONS}
        onchange={(k) => edit((p) => setTransition(p, c.id, k, tr?.ms ?? 500), k === 'cut' ? 'Remove transition' : 'Set transition')}
      />
      {#if tr?.kind === 'dip'}
        <p class="hint muted">Fades out and back in: through black on the lowest track, through the tracks below on an upper one.</p>
      {/if}
      {#if tr}
        <div class="lane">
          <span class="name">Length</span>
          <NumberField label="Transition length" value={tr.ms} min={1} max={MAX_TRANSITION_MS} step={50} unit="ms" onchange={(v) => edit((p) => setTransition(p, c.id, tr.kind, v), 'Set transition')} />
        </div>
      {/if}
    </section>
  {/if}

  <section>
    <h3>Track</h3>
    <input
      class="text"
      value={t.name}
      maxlength="120"
      aria-label="Track name"
      onchange={(e) => edit((p) => setTrack(p, t.id, { name: (e.currentTarget as HTMLInputElement).value }), 'Rename track')}
    />
    <div class="row wrap">
      <button class:on={t.muted} aria-pressed={t.muted} onclick={() => edit((p) => setTrack(p, t.id, { muted: !t.muted }), t.muted ? 'Unmute a track' : 'Mute a track')}>
        {t.muted ? 'Unmute' : 'Mute'}
      </button>
      <button title="Draw this track above the next one" onclick={() => edit((p) => moveTrack(p, t.id, 1), 'Move track')}>Up</button>
      <button title="Draw this track below" onclick={() => edit((p) => moveTrack(p, t.id, -1), 'Move track')}>Down</button>
      <button class="danger" onclick={() => {
        edit((p) => removeTrack(p, t.id), 'Remove track');
        ed.clipId = null;
      }}>Remove track</button>
    </div>
  </section>
{:else}
  <p class="muted hint">Select a clip on the timeline to change it here. Drag it to move it, drag its ends to trim, click a lane under it to add a key.</p>
{/if}


<style>
  section {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding-bottom: var(--space-3);
    margin-bottom: var(--space-3);
    border-bottom: 1px solid var(--border);
  }
  h3 {
    margin: 0;
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .row {
    display: flex;
    gap: var(--space-2);
  }
  .row.wrap {
    flex-wrap: wrap;
  }
  .grid {
    display: grid;
    grid-template-columns: 70px 1fr;
    align-items: center;
    gap: var(--space-2);
  }
  .lane {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .lane .name {
    width: 64px;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .key {
    min-width: 0;
    padding: 0 6px;
    color: var(--text-muted);
  }
  .key.on,
  .on {
    color: var(--accent-text);
  }
  .tiny {
    min-width: 0;
    padding: 0 5px;
    height: 22px;
    font-size: var(--text-xs);
  }
  .points {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .points li {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .text {
    font: inherit;
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    height: 28px;
    padding: 0 8px;
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
  }
  .mono {
    font-family: var(--font-mono);
  }
  .danger {
    color: var(--bad);
  }
</style>
