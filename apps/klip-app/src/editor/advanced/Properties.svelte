<!--
  Properties: the selected clip (where it sits, split, remove), the transition into the
  next clip on its track, and its track (name, mute, order); or the selected transition.
  Its effects and keys are in Effect Controls.
-->
<script lang="ts">
  import type { PlacedClip, Track, TransitionKind } from '../../lib/advanced';
  import {
    MAX_TRANSITION_MS,
    clipLength,
    moveClip,
    moveTrack,
    nextOnTrack,
    overlapOf,
    readAdvanced,
    removeClip,
    removeTrack,
    FROM_BELOW,
    fromBelow,
    setFromBelow,
    setTrack,
    setTransition,
    setTransitionAt,
    sourceLabel,
    splitClip,
    transitionAfter,
  } from '../../lib/advanced';
  import { ed, edit } from '../../lib/editor.svelte';
  import NumberField from '../../lib/forms/NumberField.svelte';
  import Segmented from '../../lib/forms/Segmented.svelte';
  import { formatTimecode } from '../../lib/format';
  import type { Project } from '../../lib/timeline';

  let { project, mainName, names }: { project: Project; mainName: string; names: Map<string, string> } = $props();

  const adv = $derived(readAdvanced(project));
  const selTrans = $derived(
    ed.transitionSel
      ? (adv.transitions.find(
          (x) => x.track === ed.transitionSel?.track && x.after_clip === ed.transitionSel.after && x.kind !== 'cut',
        ) ?? null)
      : null,
  );
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
      <button
        title="Split at the playhead (S)"
        onclick={() => edit((p) => void splitClip(p, c.id, c.at_ms + at), 'Split clip')}>Split</button
      >
      <button
        class="danger"
        title="Remove (Delete)"
        onclick={() => {
          edit((p) => removeClip(p, c.id), 'Remove clip');
          ed.clipId = null;
        }}>Remove</button
      >
    </div>
    <div class="grid">
      <span>Starts at</span>
      <NumberField
        label="Starts at"
        value={c.at_ms / 1000}
        min={0}
        max={86_400}
        step={0.001}
        unit="s"
        onchange={(v) => edit((p) => moveClip(p, c.id, v * 1000), 'Move clip')}
      />
    </div>
  </section>

  {#if t.kind === 'video' && t.clips[0]?.id === c.id}
    {@const fb = fromBelow(adv, t.id)}
    <section>
      <h3>From the clip before</h3>
      <Segmented
        label="Into this clip"
        value={fb ? 'crossfade' : 'cut'}
        options={TRANSITIONS.filter((x) => x.value !== 'dip')}
        onchange={(k) =>
          edit(
            (p) => setFromBelow(p, c.id, k === 'cut' ? null : (fb?.ms ?? 500)),
            k === 'cut' ? 'Remove transition' : 'Crossfade in from below',
          )}
      />
      <p class="hint muted">Fades this clip in over whatever plays below it (Main, or a lower track).</p>
      {#if fb}
        <div class="lane">
          <span class="name">Length</span>
          <NumberField
            label="Crossfade length"
            value={fb.ms}
            min={1}
            max={MAX_TRANSITION_MS}
            step={50}
            unit="ms"
            onchange={(v) => edit((p) => setFromBelow(p, c.id, v), 'Crossfade in from below')}
          />
        </div>
      {/if}
    </section>
  {/if}

  {#if next && t.kind === 'video'}
    <section>
      <h3>Into the next clip</h3>
      <Segmented
        label="Transition"
        value={tr?.kind ?? 'cut'}
        options={TRANSITIONS}
        onchange={(k) =>
          edit((p) => setTransition(p, c.id, k, tr?.ms ?? 500), k === 'cut' ? 'Remove transition' : 'Set transition')}
      />
      {#if tr?.kind === 'dip'}
        <p class="hint muted">Fades to black and back in, whatever plays below.</p>
      {/if}
      {#if tr}
        <div class="lane">
          <span class="name">Length</span>
          <NumberField
            label="Transition length"
            value={tr.ms}
            min={1}
            max={MAX_TRANSITION_MS}
            step={50}
            unit="ms"
            onchange={(v) => edit((p) => setTransition(p, c.id, tr.kind, v), 'Set transition')}
          />
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
      onchange={(e) =>
        edit((p) => setTrack(p, t.id, { name: (e.currentTarget as HTMLInputElement).value }), 'Rename track')}
    />
    <div class="row wrap">
      {#if t.kind === 'video' || t.kind === 'text'}
        <button
          class:on={t.hidden}
          aria-pressed={t.hidden}
          title="Hide the track's picture (its sound stays)"
          onclick={() =>
            edit((p) => setTrack(p, t.id, { hidden: !t.hidden }), t.hidden ? 'Show a track' : 'Hide a track')}
        >
          {t.hidden ? 'Show' : 'Hide'}
        </button>
      {/if}
      <button
        class:on={t.muted}
        aria-pressed={t.muted}
        onclick={() => edit((p) => setTrack(p, t.id, { muted: !t.muted }), t.muted ? 'Unmute a track' : 'Mute a track')}
      >
        {t.muted ? 'Unmute' : 'Mute'}
      </button>
      {#if t.main}
        <!-- V1 arranged: always at the bottom; "removing" it goes back to the simple cut. -->
        <button
          title="V1 plays Normal mode's cut again; the moves on V1 go (Ctrl+Z brings them back)"
          onclick={() => {
            edit((p) => removeTrack(p, t.id), 'Back to the simple cut');
            ed.clipId = null;
          }}>Back to the simple cut</button
        >
      {:else}
        <button
          title="Draw this track above the next one"
          onclick={() => edit((p) => moveTrack(p, t.id, 1), 'Move track')}>Up</button
        >
        <button title="Draw this track below" onclick={() => edit((p) => moveTrack(p, t.id, -1), 'Move track')}
          >Down</button
        >
        <button
          class="danger"
          onclick={() => {
            edit((p) => removeTrack(p, t.id), 'Remove track');
            ed.clipId = null;
          }}>Remove track</button
        >
      {/if}
    </div>
  </section>
{:else if selTrans}
  {@const st = selTrans}
  <section>
    <h3>{st.after_clip === FROM_BELOW ? 'Crossfade in from below' : 'Transition'}</h3>
    <Segmented
      label="Transition"
      value={st.kind}
      options={TRANSITIONS.filter(
        (x) => x.value !== 'cut' && (st.after_clip !== FROM_BELOW || x.value === 'crossfade'),
      )}
      onchange={(k) => edit((p) => setTransitionAt(p, st.track, st.after_clip, k, st.ms), 'Set transition')}
    />
    <div class="lane">
      <span class="name">Length</span>
      <NumberField
        label="Transition length"
        value={st.ms}
        min={1}
        max={MAX_TRANSITION_MS}
        step={50}
        unit="ms"
        onchange={(v) => edit((p) => setTransitionAt(p, st.track, st.after_clip, st.kind, v), 'Set transition')}
      />
    </div>
    <p class="hint muted">Drag its ends on the timeline to change the length.</p>
    <div class="row">
      <button
        class="danger"
        title="Remove (Delete)"
        onclick={() => {
          edit((p) => setTransitionAt(p, st.track, st.after_clip, 'cut', 0), 'Remove a transition');
          ed.transitionSel = null;
        }}>Remove</button
      >
    </div>
  </section>
{:else}
  <p class="muted hint">
    Select a clip or a transition on the timeline to change it here. A click selects; drag a clip to move it, drag its
    ends to trim; the + between two touching clips adds a crossfade.
  </p>
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
  .on {
    color: var(--accent-text);
  }
  .text {
    font: inherit;
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    height: var(--control-h);
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
