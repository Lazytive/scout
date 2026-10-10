<!--
  The export dialog: pick a preset, watch the passes go by, then "Show in folder".
  Everything it shows comes from the editor store (export.start / export_progress /
  export_done); closing while an export runs leaves it running.
-->
<script lang="ts">
  import { EXPORT_PRESETS, progressPct } from '../lib/editor';
  import type { ExportPreset } from '../lib/editor';
  import { cancelExport, ed, revealPath, startExport } from '../lib/editor.svelte';
  import { formatBytes, formatDuration } from '../lib/format';

  let { limitMb, onclose }: { limitMb: number; onclose: () => void } = $props();

  let preset = $state<ExportPreset>('youtube');
  let dialog = $state<HTMLDivElement | null>(null);

  const run = $derived(ed.exporting);
  const result = $derived(ed.exportResult);
  const summary = $derived((result?.summary ?? null) as { bytes?: number; duration_ms?: number; width?: number; height?: number; fps?: number; passes?: number } | null);

  $effect(() => {
    dialog?.focus();
  });

  function keydown(e: KeyboardEvent) {
    if (e.key === 'Escape') onclose();
  }
</script>

<div class="scrim" role="presentation" onclick={onclose}>
  <div
    class="dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="export-title"
    tabindex="-1"
    bind:this={dialog}
    onclick={(e) => e.stopPropagation()}
    onkeydown={keydown}
  >
    <h2 id="export-title">Export</h2>
    <div class="presets" role="radiogroup" aria-label="Preset">
      {#each EXPORT_PRESETS as p (p.id)}
        <label class="preset" class:on={preset === p.id}>
          <input type="radio" name="preset" value={p.id} checked={preset === p.id} disabled={run !== null} onchange={() => (preset = p.id)} />
          <span class="name">{p.label}</span>
          <span class="detail muted">{p.id === 'discord' ? `H.264 under ${limitMb} MB (Settings → Export)` : p.detail}</span>
        </label>
      {/each}
    </div>

    {#if run}
      <div class="progress" role="status">
        <div class="meter" aria-hidden="true">
          <div class="bar" style:width="{progressPct(run.frames_done, run.frames_total)}%"></div>
        </div>
        <span class="mono">
          {#if run.pass > 1}pass {run.pass} · {/if}{run.frames_total > 0 ? `${progressPct(run.frames_done, run.frames_total)} %` : 'starting…'}
        </span>
      </div>
    {:else if result}
      {#if result.ok && result.path}
        <p class="done">Saved <span class="mono">{result.path}</span></p>
        {#if summary}
          <p class="muted">
            {#if summary.width && summary.height}{summary.width}×{summary.height}{/if}
            {#if summary.fps} · {summary.fps} fps{/if}
            {#if summary.duration_ms} · {formatDuration(summary.duration_ms)}{/if}
            {#if summary.bytes} · {formatBytes(summary.bytes)}{/if}
            {#if summary.passes && summary.passes > 1} · {summary.passes} passes{/if}
          </p>
        {/if}
      {:else}
        <p class="error" role="alert">{result.error ?? 'The export failed.'}</p>
      {/if}
    {/if}
    {#if ed.exportError}<p class="error" role="alert">{ed.exportError}</p>{/if}

    <div class="buttons">
      {#if run}
        <button onclick={() => void cancelExport()}>Cancel export</button>
      {:else if result?.ok && result.path}
        <button onclick={() => void revealPath(result.path ?? '')}>Show in folder</button>
      {/if}
      <span class="grow"></span>
      <button onclick={onclose}>Close</button>
      <button class="primary" disabled={run !== null} onclick={() => void startExport(preset)}>
        {result ? 'Export again' : 'Export'}
      </button>
    </div>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 45%);
    z-index: 30;
    display: grid;
    place-items: center;
  }
  .dialog {
    width: 520px;
    max-width: calc(100vw - 32px);
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: var(--space-4);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    outline: none;
  }
  h2 {
    margin: 0;
    font-size: var(--text-lg);
    color: var(--text-strong);
  }
  .presets {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
  }
  .preset {
    display: grid;
    grid-template-columns: auto 90px 1fr;
    gap: var(--space-2);
    align-items: center;
    padding: var(--space-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  .preset.on {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }
  .detail {
    font-size: var(--text-sm);
  }
  .progress {
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }
  .meter {
    flex: 1;
    height: 8px;
    border-radius: 4px;
    background: var(--panel-raised);
    overflow: hidden;
  }
  .bar {
    height: 100%;
    background: var(--accent);
    transition: width var(--motion);
  }
  .done {
    margin: 0;
    word-break: break-all;
  }
  .error {
    margin: 0;
    color: var(--bad);
    font-size: var(--text-sm);
  }
  .buttons {
    display: flex;
    gap: var(--space-2);
    align-items: center;
  }
  .grow {
    flex: 1;
  }
</style>
