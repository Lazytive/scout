<!--
  The Editor workspace. Normal mode (ADR 0017): tools on the left (format, speed,
  split/cut/trim, text presets, volumes, templates, mode), the preview and the
  filmstrip in the middle, the export on the right. Advanced mode (ADR 0018, M9.5):
  `editor/advanced/Workspace.svelte`. The mode switches live; Normal mode keeps
  Advanced edits and says so with a badge. Every edit is saved to the engine after a
  moment; Ctrl+Z / Ctrl+Y undo and redo; Space plays; S splits; Delete cuts the
  selection; arrow keys step a frame.
-->
<script lang="ts">
  import { onMount } from 'svelte';

  import Workspace from './editor/advanced/Workspace.svelte';
  import ExportDialog from './editor/ExportDialog.svelte';
  import Filmstrip from './editor/Filmstrip.svelte';
  import NormalTools from './editor/NormalTools.svelte';
  import Preview from './editor/Preview.svelte';
  import type { Overlay } from './editor/Preview.svelte';
  import { hasAdvancedEdits } from './lib/advanced';
  import { call, thumbUrl } from './lib/api';
  import { outputSize } from './lib/editor';
  import {
    closeEditor,
    cutSelection,
    ed,
    edit,
    editDuration,
    redo,
    removeText,
    requestMedia,
    seek,
    select,
    setCropX,
    splitAtPlayhead,
    trimEnd,
    trimStart,
    undo,
    updateText,
  } from './lib/editor.svelte';
  import { formatTimecode } from './lib/format';
  import Panel from './lib/Panel.svelte';
  import { app } from './lib/store.svelte';
  import { outputDurationMs } from './lib/timeline';

  const clip = $derived(ed.clip);
  const project = $derived(ed.project);
  const source = $derived(ed.source);
  const advanced = $derived(project?.mode === 'advanced');
  const duration = $derived(project ? outputDurationMs(project) : 0);
  const strip = $derived(clip?.thumbs.strip ? thumbUrl('strip', clip.key, clip.thumbs.strip) : null);
  const limitMb = $derived(app.settings?.export.discord_limit_mb ?? 10);
  const frameMs = $derived(source?.fps ? Math.max(1, Math.round(1000 / source.fps)) : 17);
  const badge = $derived(project !== null && !advanced && hasAdvancedEdits(project));

  function setMode(mode: 'normal' | 'advanced') {
    if (!project || project.mode === mode) return;
    ed.playing = false;
    edit((p) => {
      p.mode = mode;
    }, mode === 'advanced' ? 'Advanced mode' : 'Normal mode');
  }

  // Text cues as the engine draws them, re-rendered when a layer or the format changes.
  let renderTimer: ReturnType<typeof setTimeout> | null = null;
  const renderKey = $derived(project && source ? JSON.stringify([project.text, project.format, source.width, source.height]) : '');
  $effect(() => {
    const key = renderKey;
    if (!key || !clip || !project || !source) {
      ed.cues = [];
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
    if (renderKey === key) ed.cues = next;
  }

  function keydown(e: KeyboardEvent) {
    if (advanced) return;
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
        if (ed.exportOpen) ed.exportOpen = false;
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

{#snippet exportFacts()}
  {#if project && source}
    {@const out = outputSize(project.format.kind, project.format.crop_x, source.width, source.height)}
    <dl class="facts">
      <dt>Length</dt>
      <dd class="mono">{formatTimecode(advanced ? editDuration() : duration)}</dd>
      {#if advanced}
        <dt>Clips</dt>
        <dd class="mono">{1 + ed.clips.length}</dd>
      {:else}
        <dt>Pieces</dt>
        <dd class="mono">{Math.max(1, project.pieces.length)}</dd>
      {/if}
      <dt>Text</dt>
      <dd class="mono">{project.text.length}</dd>
      <dt>Output</dt>
      <dd class="mono">{out.width}×{out.height}</dd>
    </dl>
    {#if ed.clips.some((c) => c.status !== 'ok')}
      <p class="error" role="alert">
        Missing: {ed.clips.filter((c) => c.status !== 'ok').map((c) => c.name).join(', ')}. Put them back (or remove them) to export.
      </p>
    {/if}
    <button class="primary wide" onclick={() => (ed.exportOpen = true)}>Export…</button>
    {#if ed.exporting}
      <p class="muted">Exporting ({ed.exporting.preset})…</p>
    {:else if ed.exportResult?.ok}
      <p class="muted">Saved as <span class="mono">{ed.exportResult.path}</span></p>
    {/if}
    {#if ed.issues.length > 0}
      <p class="hint muted">Repaired: {ed.issues.join('; ')}</p>
    {/if}
  {/if}
{/snippet}

{#if project && source && advanced}
  <Workspace {project} onmode={setMode} {exportFacts} />
{:else}
  <div class="workspace">
    <div class="left">
      <Panel tabs={['Edit']}>
        {#if project && source}
          <NormalTools {project} />
          <section>
            <h3>Mode</h3>
            <div class="mode" role="group" aria-label="Mode">
              <button class="flat on" aria-pressed="true">Normal</button>
              <button class="flat" onclick={() => setMode('advanced')}>Advanced</button>
            </div>
            {#if badge}
              <p class="badge" title="Tracks, keyframes, a montage, colour or markers made in Advanced mode. They are kept and exported; switch to Advanced to see them.">
                Advanced edits
              </p>
            {:else}
              <p class="hint muted">Advanced: tracks, keyframes, montages, stickers, music, colour.</p>
            {/if}
          </section>
        {:else if ed.loading}
          <p class="muted">Loading…</p>
        {/if}
      </Panel>
    </div>

    <div class="center">
      <Panel tabs={[clip?.name ?? 'Editor']}>
        {#snippet actions()}
          {#if badge}<span class="badge small" title="This project has Advanced-mode edits; they are kept and exported">Advanced edits</span>{/if}
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
              overlays={ed.cues}
              tracks={ed.tracks.flatMap((t) => (t.url ? [{ name: t.name, url: t.url }] : []))}
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
              <button class="flat" disabled={ed.undo.length === 0} onclick={() => undo()} title="Undo (Ctrl+Z)">Undo</button>
              <button class="flat" disabled={ed.redo.length === 0} onclick={() => redo()} title="Redo (Ctrl+Y)">Redo</button>
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
        {@render exportFacts()}
        {#if ed.error}<p class="error" role="alert">{ed.error}</p>{/if}
      </Panel>
    </div>
  </div>
{/if}

{#if ed.exportOpen}
  <ExportDialog {limitMb} onclose={() => (ed.exportOpen = false)} />
{/if}

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
  .hint {
    margin: 0;
    font-size: var(--text-sm);
  }
  .mode {
    display: inline-flex;
    align-self: flex-start;
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    overflow: hidden;
  }
  .mode .on {
    background: var(--accent);
    color: var(--on-accent);
  }
  .badge {
    margin: 0;
    align-self: flex-start;
    padding: 2px 8px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--warn) 22%, transparent);
    color: var(--warn);
    font-size: var(--text-sm);
    font-weight: 600;
  }
  .badge.small {
    font-size: var(--text-xs);
    padding: 1px 6px;
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
  .mono {
    font-family: var(--font-mono);
  }
</style>
