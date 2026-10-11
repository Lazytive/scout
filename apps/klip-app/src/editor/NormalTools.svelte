<!--
  Normal mode's tools for the main clip (ADR 0017): format, cut, speed, text cues,
  the Game/Mic sound, templates. Advanced mode shows them as its "Main clip" tab: they
  edit the cut that is the Main track there.
-->
<script lang="ts">
  import { FORMATS, POSITIONS, SPEED_OPTIONS, TEXT_PRESETS, trimText } from '../lib/editor';
  import {
    addText,
    applyTemplate,
    edit,
    cutSelection,
    ed,
    removeText,
    setFormat,
    setVolumes,
    speedSelection,
    splitAtPlayhead,
    trimToSelection,
    updateText,
  } from '../lib/editor.svelte';
  import Segmented from '../lib/forms/Segmented.svelte';
  import Select from '../lib/forms/Select.svelte';
  import Slider from '../lib/forms/Slider.svelte';
  import { formatTimecode } from '../lib/format';
  import type { Project, Speed } from '../lib/timeline';
  import { mainArranged, useSimpleCut } from '../lib/advanced';

  let { project, advanced = false }: { project: Project; advanced?: boolean } = $props();

  const selectedText = $derived(project.text.find((t) => t.id === ed.textId) ?? null);
</script>

<section>
  <h3>Format</h3>
  <Segmented value={project.format.kind} options={FORMATS} label="Format" onchange={(v) => setFormat(v)} />
  {#if project.format.kind === 'tall' && !advanced}
    <p class="hint muted">Drag the window on the preview to choose what the 9:16 shows.</p>
  {/if}
</section>
{#if !advanced}
  <section>
    <h3>Cut</h3>
    <div class="row">
      <button onclick={splitAtPlayhead} title="Split at the playhead (S)">Split</button>
      <button disabled={!ed.selection} onclick={cutSelection} title="Remove the selection (Delete)">Cut</button>
      <button disabled={!ed.selection} onclick={trimToSelection} title="Keep only the selection">Keep</button>
    </div>
    <p class="hint muted">
      {#if ed.selection}
        Selected {formatTimecode(ed.selection.start)} – {formatTimecode(ed.selection.end)}
      {:else}
        Drag on the filmstrip to select; drag its ends to trim.
      {/if}
    </p>
  </section>
  <section>
    <h3>Speed</h3>
    <div class="row">
      {#each SPEED_OPTIONS as s (s.value)}
        <button disabled={!ed.selection} onclick={() => speedSelection(s.value as Speed)}>{s.label}</button>
      {/each}
    </div>
    <p class="hint muted">Applies to the selection; the pitch stays.</p>
  </section>
{:else}
  <section>
    <h3>Cut</h3>
    {#if mainArranged(project)}
      <p class="hint muted">Your clip is arranged on V1: its pieces move like any clip there. Normal mode's simple cut is set aside (and Normal mode with it).</p>
      <div class="row">
        <button onclick={() => edit((p) => useSimpleCut(p), 'Back to the simple cut')} title="V1 plays Normal mode's cut again; the moves on V1 go (Ctrl+Z brings them back)">Back to the simple cut</button>
      </div>
    {:else}
      <div class="row">
        <button onclick={splitAtPlayhead} title="Split the main clip at the playhead">Split main</button>
      </div>
      <p class="hint muted">V1 is this cut. Trim and cut it in Normal mode, or unlock V1 on the timeline to move its pieces like any clip.</p>
    {/if}
  </section>
{/if}
<section>
  <h3>Text</h3>
  <div class="row wrap">
    {#each TEXT_PRESETS as t (t.id)}
      <button onclick={() => addText(t.id)} title={t.hint}>+ {t.label}</button>
    {/each}
  </div>
  {#if selectedText}
    <div class="text-edit">
      <textarea
        rows="2"
        maxlength="200"
        value={selectedText.text}
        aria-label="Text"
        oninput={(e) => updateText(selectedText.id, { text: trimText((e.currentTarget as HTMLTextAreaElement).value) })}
      ></textarea>
      <Select
        label="Style"
        value={selectedText.preset}
        options={TEXT_PRESETS.map((t) => ({ value: t.id, label: t.label }))}
        onchange={(v) => updateText(selectedText.id, { preset: String(v) })}
      />
      <Segmented value={selectedText.position} options={POSITIONS} label="Position" onchange={(v) => updateText(selectedText.id, { position: v })} />
      <p class="hint muted mono">{formatTimecode(selectedText.start_ms)} – {formatTimecode(selectedText.end_ms)}</p>
      <div class="row">
        <button onclick={() => updateText(selectedText.id, { start_ms: ed.playhead, end_ms: ed.playhead + (selectedText.end_ms - selectedText.start_ms) })}>
          Start here
        </button>
        <button onclick={() => updateText(selectedText.id, { end_ms: Math.max(ed.playhead + 1, selectedText.start_ms + 1) })}>End here</button>
        <button class="danger" onclick={() => removeText(selectedText.id)}>Remove</button>
      </div>
    </div>
  {:else if project.text.length > 0}
    {#if advanced}
      <div class="row wrap">
        {#each project.text as t (t.id)}
          <button class="flat chip" title="Edit this text" onclick={() => (ed.textId = t.id)}>{t.text}</button>
        {/each}
      </div>
    {:else}
      <p class="hint muted">Click a cue on the filmstrip to edit it.</p>
    {/if}
  {/if}
</section>
<section>
  <h3>Sound</h3>
  <Slider label="Game" value={project.volumes.game} min={0} max={2} step={0.05} format={(v) => `${Math.round(v * 100)} %`} onchange={(v) => setVolumes(v, project.volumes.mic)} />
  <Slider label="Mic" value={project.volumes.mic} min={0} max={2} step={0.05} format={(v) => `${Math.round(v * 100)} %`} onchange={(v) => setVolumes(project.volumes.game, v)} />
</section>
{#if !advanced}
  <section>
    <h3>Templates</h3>
    <button onclick={() => applyTemplate('shorts')} title="9:16, centred, the clip's name as a caption">Shorts</button>
  </section>
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
  }
  .row {
    display: flex;
    gap: var(--space-2);
  }
  .row.wrap {
    flex-wrap: wrap;
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
  }
  .text-edit {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
  }
  textarea {
    width: 100%;
    resize: vertical;
    background: var(--input);
    color: var(--text);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    padding: var(--space-2);
    font: inherit;
    user-select: text;
  }
  .chip {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    border: 1px solid var(--border);
  }
  .mono {
    font-family: var(--font-mono);
  }
  .danger {
    color: var(--bad);
  }
</style>
