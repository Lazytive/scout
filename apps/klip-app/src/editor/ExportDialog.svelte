<!--
  The export dialog: pick a preset (or Custom: size, frame rate, bitrate, codec, kept
  per viewer), watch the passes go by, then "Show in folder".
  Everything it shows comes from the editor store (export.start / export_progress /
  export_done); closing while an export runs leaves it running.
-->
<script lang="ts">
  import { CUSTOM_CODECS, CUSTOM_LIMITS, EXPORT_PRESETS, clampCustom, keepAspect, outputSize, progressPct } from '../lib/editor';
  import type { CustomSettings, ExportPreset } from '../lib/editor';
  import { cancelExport, ed, revealPath, startExport } from '../lib/editor.svelte';
  import NumberField from '../lib/forms/NumberField.svelte';
  import Segmented from '../lib/forms/Segmented.svelte';
  import { formatBytes, formatDuration } from '../lib/format';

  let { limitMb, onclose }: { limitMb: number; onclose: () => void } = $props();

  const CUSTOM_KEY = 'klip.export.custom';
  /** The edit's own size and rate: where Custom starts. */
  const base = $derived.by(() => {
    const p = ed.project;
    const s = ed.source;
    const size = p && s ? outputSize(p.format.kind, p.format.crop_x, s.width, s.height) : { width: 1920, height: 1080 };
    return { ...size, fps: Math.round(s?.fps ?? 60) || 60 };
  });

  function loadCustom(): CustomSettings {
    const d: CustomSettings = { width: base.width, height: base.height, fps: base.fps, bitrate_kbps: 20_000, codec: 'h264' };
    try {
      const raw = JSON.parse(localStorage.getItem(CUSTOM_KEY) ?? 'null') as Partial<CustomSettings> | null;
      if (raw && typeof raw === 'object') return clampCustom({ ...d, ...raw, width: d.width, height: d.height });
    } catch {
      // Defaults.
    }
    return d;
  }

  let preset = $state<ExportPreset | 'custom'>('youtube');
  let custom = $state<CustomSettings>(loadCustom());
  function setCustom(patch: Partial<CustomSettings>, changed?: 'width' | 'height') {
    let next = { ...custom, ...patch };
    // The export fits the edit into the size, so the other side follows its shape.
    if (changed) {
      const other = keepAspect(changed === 'width' ? next.width : next.height, base.width, base.height, changed);
      next = changed === 'width' ? { ...next, height: other } : { ...next, width: other };
    }
    custom = clampCustom(next);
    try {
      localStorage.setItem(CUSTOM_KEY, JSON.stringify({ fps: custom.fps, bitrate_kbps: custom.bitrate_kbps, codec: custom.codec }));
    } catch {
      // Not kept; fine.
    }
  }
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
      <label class="preset" class:on={preset === 'custom'}>
        <input type="radio" name="preset" value="custom" checked={preset === 'custom'} disabled={run !== null} onchange={() => (preset = 'custom')} />
        <span class="name">Custom</span>
        <span class="detail muted">{custom.width}×{custom.height} · {custom.fps} fps · {Math.round(custom.bitrate_kbps / 100) / 10} Mbit/s · {CUSTOM_CODECS.find((c) => c.value === custom.codec)?.label}</span>
      </label>
    </div>

    {#if preset === 'custom'}
      <div class="custom">
        <span class="label">Size</span>
        <span class="pair">
          <NumberField label="Width" value={custom.width} min={CUSTOM_LIMITS.side[0]} max={CUSTOM_LIMITS.side[1]} step={2} unit="×" disabled={run !== null} onchange={(v) => setCustom({ width: v }, 'width')} />
          <NumberField label="Height" value={custom.height} min={CUSTOM_LIMITS.side[0]} max={CUSTOM_LIMITS.side[1]} step={2} disabled={run !== null} onchange={(v) => setCustom({ height: v }, 'height')} />
        </span>
        <span></span>
        <span class="muted hint">The edit keeps its shape inside this size.</span>
        <span class="label">Frame rate</span>
        <NumberField label="Frame rate" value={custom.fps} min={CUSTOM_LIMITS.fps[0]} max={CUSTOM_LIMITS.fps[1]} step={1} unit="fps" disabled={run !== null} onchange={(v) => setCustom({ fps: v })} />
        <span class="label">Bitrate</span>
        <NumberField label="Bitrate" value={custom.bitrate_kbps} min={CUSTOM_LIMITS.kbps[0]} max={CUSTOM_LIMITS.kbps[1]} step={500} unit="kbit/s" disabled={run !== null} onchange={(v) => setCustom({ bitrate_kbps: v })} />
        <span class="label">Codec</span>
        <Segmented label="Codec" value={custom.codec} options={CUSTOM_CODECS} onchange={(v) => setCustom({ codec: v })} />
      </div>
      <p class="muted hint">HEVC and AV1 need a graphics card that encodes them; the export says so if yours doesn't.</p>
    {/if}

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
      <button class="primary" disabled={run !== null} onclick={() => void (preset === 'custom' ? startExport('custom', custom) : startExport(preset))}>
        {result ? 'Export again' : 'Export'}
      </button>
    </div>
  </div>
</div>

<style>
  .custom {
    display: grid;
    grid-template-columns: 90px 1fr;
    align-items: center;
    gap: var(--space-2) var(--space-3);
    margin: var(--space-2) 0;
  }
  .custom .label {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .pair {
    display: inline-flex;
    gap: var(--space-2);
    align-items: center;
  }
  .hint {
    margin: 0 0 var(--space-2);
    font-size: var(--text-sm);
  }
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
