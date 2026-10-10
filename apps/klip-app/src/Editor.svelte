<!--
  The Editor workspace (ADR 0017, Normal mode): tools on the left (format, speed,
  split/cut/trim, text presets, volumes, templates, mode), the preview and the
  filmstrip in the middle, the export on the right. Every edit is saved to the engine
  after a moment; Ctrl+Z / Ctrl+Y undo and redo; Space plays; S splits; Delete cuts the
  selection; arrow keys step a frame.
-->
<script lang="ts">
  import { onMount } from 'svelte';

  import ExportDialog from './editor/ExportDialog.svelte';
  import Filmstrip from './editor/Filmstrip.svelte';
  import Preview from './editor/Preview.svelte';
  import type { Overlay } from './editor/Preview.svelte';
  import { call, thumbUrl } from './lib/api';
  import { FORMATS, POSITIONS, SPEED_OPTIONS, TEXT_PRESETS, outputSize, trimText } from './lib/editor';
  import {
    addText,
    applyTemplate,
    closeEditor,
    cutSelection,
    ed,
    redo,
    removeText,
    requestMedia,
    seek,
    select,
    setCropX,
    setFormat,
    setVolumes,
    speedSelection,
    splitAtPlayhead,
    trimEnd,
    trimStart,
    trimToSelection,
    undo,
    updateText,
  } from './lib/editor.svelte';
  import Segmented from './lib/forms/Segmented.svelte';
  import Select from './lib/forms/Select.svelte';
  import Slider from './lib/forms/Slider.svelte';
  import { formatTimecode } from './lib/format';
  import Panel from './lib/Panel.svelte';
  import { app } from './lib/store.svelte';
  import type { Speed } from './lib/timeline';
  import { outputDurationMs } from './lib/timeline';

  const clip = $derived(ed.clip);
  const project = $derived(ed.project);
  const source = $derived(ed.source);
  const duration = $derived(project ? outputDurationMs(project) : 0);
  const strip = $derived(clip?.thumbs.strip ? thumbUrl('strip', clip.key, clip.thumbs.strip) : null);
  const selectedText = $derived(project?.text.find((t) => t.id === ed.textId) ?? null);
  const limitMb = $derived(app.settings?.export.discord_limit_mb ?? 10);
  const frameMs = $derived(source?.fps ? Math.max(1, Math.round(1000 / source.fps)) : 17);

  // Text cues as the engine draws them, re-rendered when a layer or the format changes.
  let overlays = $state<Overlay[]>([]);
  let renderTimer: ReturnType<typeof setTimeout> | null = null;
  const renderKey = $derived(
    project && source ? JSON.stringify([project.text, project.format, source.width, source.height]) : '',
  );
  $effect(() => {
    const key = renderKey;
    if (!key || !clip || !project || !source) {
      overlays = [];
      return;
    }
    if (renderTimer) clearTimeout(renderTimer);
    renderTimer = setTimeout(() => void renderOverlays(clip.path, key), 250);
  });

  async function renderOverlays(path: string, key: string) {
    const p = ed.project;
    const s = ed.source;
    if (!p || !s) return;
    const size = outputSize(p.format.kind, p.format.crop_x, s.width, s.height);
    const next: Overlay[] = [];
    for (const layer of p.text) {
      try {
        const r = await call<{
          x: number;
          y: number;
          width: number;
          height: number;
          frame_width: number;
          frame_height: number;
          png: string | null;
          url?: string;
        }>('editor.text.render', { path, layer, width: size.width, height: size.height });
        const url = r.url ?? (r.png ? `data:image/png;base64,${r.png}` : null);
        if (url) next.push({ id: layer.id, url, ...r, start_ms: layer.start_ms, end_ms: layer.end_ms });
      } catch {
        // A layer that can't be drawn is simply not shown.
      }
    }
    if (renderKey === key) overlays = next;
  }

  let exportOpen = $state(false);

  function keydown(e: KeyboardEvent) {
    const target = e.target as HTMLElement | null;
    if (target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA' || target.tagName === 'SELECT')) return;
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z') {
      e.preventDefault();
      if (e.shiftKey) redo();
      else undo();
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'y') {
      e.preventDefault();
      redo();
      return;
    }
    switch (e.key) {
      case ' ':
        e.preventDefault();
        ed.playing = !ed.playing;
        break;
      case 's':
      case 'S':
        splitAtPlayhead();
        break;
      case 'Delete':
      case 'Backspace':
        if (ed.textId !== null && !ed.selection) removeText(ed.textId);
        else cutSelection();
        break;
      case 'ArrowLeft':
        e.preventDefault();
        ed.playing = false;
        seek(ed.playhead - (e.shiftKey ? 1000 : frameMs));
        break;
      case 'ArrowRight':
        e.preventDefault();
        ed.playing = false;
        seek(ed.playhead + (e.shiftKey ? 1000 : frameMs));
        break;
      case 'Home':
        seek(0);
        break;
      case 'End':
        seek(duration - 1);
        break;
      case 'Escape':
        if (exportOpen) exportOpen = false;
        else if (ed.selection) ed.selection = null;
        else ed.textId = null;
        break;
    }
  }

  onMount(() => {
    if (ed.clip && !ed.project) void requestMedia();
  });
</script>

<svelte:window onkeydown={keydown} />

<div class="workspace">
  <div class="left">
    <Panel tabs={['Edit']}>
        {#if project && source}
          <section>
            <h3>Format</h3>
            <Segmented value={project.format.kind} options={FORMATS} label="Format" onchange={(v) => setFormat(v)} />
            {#if project.format.kind === 'tall'}
              <p class="hint muted">Drag the window on the preview to choose what the 9:16 shows.</p>
            {/if}
          </section>
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
                <Segmented
                  value={selectedText.position}
                  options={POSITIONS}
                  label="Position"
                  onchange={(v) => updateText(selectedText.id, { position: v })}
                />
                <p class="hint muted mono">
                  {formatTimecode(selectedText.start_ms)} – {formatTimecode(selectedText.end_ms)}
                </p>
                <div class="row">
                  <button onclick={() => updateText(selectedText.id, { start_ms: ed.playhead, end_ms: ed.playhead + (selectedText.end_ms - selectedText.start_ms) })}>
                    Start here
                  </button>
                  <button onclick={() => updateText(selectedText.id, { end_ms: Math.max(ed.playhead + 1, selectedText.start_ms + 1) })}>End here</button>
                  <button class="danger" onclick={() => removeText(selectedText.id)}>Remove</button>
                </div>
              </div>
            {:else if project.text.length > 0}
              <p class="hint muted">Click a cue on the filmstrip to edit it.</p>
            {/if}
          </section>
          <section>
            <h3>Sound</h3>
            <Slider label="Game" value={project.volumes.game} min={0} max={2} step={0.05} format={(v) => `${Math.round(v * 100)} %`} onchange={(v) => setVolumes(v, project.volumes.mic)} />
            <Slider label="Mic" value={project.volumes.mic} min={0} max={2} step={0.05} format={(v) => `${Math.round(v * 100)} %`} onchange={(v) => setVolumes(project.volumes.game, v)} />
          </section>
          <section>
            <h3>Templates</h3>
            <button onclick={() => applyTemplate('shorts')} title="9:16, centred, the clip's name as a caption">Shorts</button>
          </section>
          <section>
            <h3>Mode</h3>
            <Segmented
              value={project.mode}
              options={[
                { value: 'normal', label: 'Normal' },
                { value: 'advanced', label: 'Advanced' },
              ]}
              label="Mode"
              onchange={() => {}}
            />
            <p class="hint muted">Advanced mode (tracks, keyframes, montage) comes with M9.</p>
          </section>
        {:else if ed.loading}
          <p class="muted">Loading…</p>
        {/if}
    </Panel>
  </div>

  <div class="center">
    <Panel tabs={[clip?.name ?? 'Editor']}>
      {#snippet actions()}
        <button class="flat" onclick={closeEditor}>← Library</button>
      {/snippet}
        {#if clip && project && source}
          <div class="stage">
            <Preview
              src={ed.media.url}
              {strip}
              tiles={ed.tiles}
              {source}
              {project}
              playhead={ed.playhead}
              playing={ed.playing}
              {overlays}
              onplayhead={(ms) => (ed.playhead = ms)}
              onended={() => (ed.playing = false)}
              oncrop={setCropX}
            />
            <div class="transport">
              <button class="primary" onclick={() => (ed.playing = !ed.playing)} title="Play / pause (Space)">
                {ed.playing ? 'Pause' : 'Play'}
              </button>
              <span class="timecode mono">{formatTimecode(ed.playhead)}</span>
              <span class="muted mono">/ {formatTimecode(duration)}</span>
              <span class="grow"></span>
              {#if ed.media.status === 'making'}<span class="muted">Making a preview…</span>{/if}
              {#if ed.saving}<span class="muted">Saving…</span>{:else if ed.dirty}<span class="muted">Edited</span>{/if}
              <button class="flat" disabled={ed.undo.length === 0} onclick={undo} title="Undo (Ctrl+Z)">Undo</button>
              <button class="flat" disabled={ed.redo.length === 0} onclick={redo} title="Redo (Ctrl+Y)">Redo</button>
            </div>
            <Filmstrip
              {strip}
              tiles={ed.tiles}
              {source}
              {project}
              playhead={ed.playhead}
              selection={ed.selection}
              textId={ed.textId}
              onseek={(ms) => {
                ed.playing = false;
                seek(ms);
              }}
              onselect={(a, b) => select(a, b)}
              ontrimstart={trimStart}
              ontrimend={trimEnd}
              ontextmove={(id, start) => {
                const t = project.text.find((x) => x.id === id);
                if (t) updateText(id, { start_ms: start, end_ms: start + (t.end_ms - t.start_ms) });
              }}
              onpicktext={(id) => (ed.textId = id)}
            />
          </div>
        {:else if ed.error}
          <p class="error" role="alert">{ed.error}</p>
        {:else}
          <p class="muted">Loading…</p>
        {/if}
    </Panel>
  </div>

  <div class="right">
    <Panel tabs={['Export']}>
        {#if project && source}
          <dl class="facts">
            <dt>Length</dt>
            <dd class="mono">{formatTimecode(duration)}</dd>
            <dt>Pieces</dt>
            <dd class="mono">{Math.max(1, project.pieces.length)}</dd>
            <dt>Text</dt>
            <dd class="mono">{project.text.length}</dd>
            <dt>Output</dt>
            <dd class="mono">
              {outputSize(project.format.kind, project.format.crop_x, source.width, source.height).width}×{outputSize(
                project.format.kind,
                project.format.crop_x,
                source.width,
                source.height,
              ).height}
            </dd>
          </dl>
          <button class="primary wide" onclick={() => (exportOpen = true)}>Export…</button>
          {#if ed.exporting}
            <p class="muted">Exporting ({ed.exporting.preset})…</p>
          {:else if ed.exportResult?.ok}
            <p class="muted">Saved as <span class="mono">{ed.exportResult.path}</span></p>
          {/if}
          {#if ed.issues.length > 0}
            <p class="hint muted">Repaired: {ed.issues.join('; ')}</p>
          {/if}
          {#if ed.error}<p class="error" role="alert">{ed.error}</p>{/if}
        {/if}
    </Panel>
  </div>

  {#if exportOpen}
    <ExportDialog {limitMb} onclose={() => (exportOpen = false)} />
  {/if}
</div>

<style>
  .workspace {
    display: grid;
    grid-template-columns: 250px minmax(0, 1fr) 260px;
    gap: 2px;
    height: 100%;
    outline: none;
  }
  .left,
  .center,
  .right {
    display: flex;
    min-height: 0;
    min-width: 0;
  }
  .left > :global(*),
  .center > :global(*),
  .right > :global(*) {
    flex: 1;
  }
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
  .stage {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    height: 100%;
  }
  .transport {
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }
  .timecode {
    font-size: var(--text-timecode);
    color: var(--accent-text);
  }
  .grow {
    flex: 1;
  }
  .facts {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: var(--space-1) var(--space-3);
    margin: 0 0 var(--space-3);
    font-size: var(--text-sm);
  }
  .facts dt {
    color: var(--text-muted);
  }
  .facts dd {
    margin: 0;
  }
  .wide {
    width: 100%;
  }
  .error {
    color: var(--bad);
    font-size: var(--text-sm);
  }
  .danger {
    color: var(--bad);
  }
</style>
