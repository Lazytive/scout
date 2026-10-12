<!-- First-run setup: GPU and encoder, quality, clip hotkey, microphone, clip folder,
     overlay. Each step saves as it goes; Skip or Finish marks the setup done. -->
<script lang="ts">
  import { onDestroy } from 'svelte';

  import { call } from './lib/api';
  import { formatBytes, formatDuration } from './lib/format';
  import FolderField from './lib/forms/FolderField.svelte';
  import KeyCapture from './lib/forms/KeyCapture.svelte';
  import Meter from './lib/forms/Meter.svelte';
  import Segmented from './lib/forms/Segmented.svelte';
  import Select from './lib/forms/Select.svelte';
  import Toggle from './lib/forms/Toggle.svelte';
  import { latestOnly } from './lib/latest';
  import { presets, type Settings } from './lib/settings';
  import { app, fieldError, meter, save } from './lib/store.svelte';

  let { s }: { s: Settings } = $props();

  const steps = ['Your PC', 'Quality', 'Hotkey', 'Microphone', 'Clip folder', 'Pop-up'] as const;
  let step = $state(0);

  // Step 1: GPU and encoder (probing can take a few seconds the first time).
  let hw = $state<{
    gpus: { name: string }[];
    encoders: { element: string; description: string }[];
    error?: string;
  } | null>(null);
  void call<NonNullable<typeof hw>>('encoders.list', { codec: 'h264' })
    .then((r) => (hw = r))
    .catch((e) => (hw = { gpus: [], encoders: [], error: e instanceof Error ? e.message : String(e) }));

  let estimate = $state<number | null>(null);
  const takeEstimate = latestOnly();
  $effect(() => {
    const draft = { recording: s.recording, audio: s.audio };
    takeEstimate(
      call<{ bytes: number }>('recording.estimate', draft),
      (r) => (estimate = r.bytes),
      () => (estimate = null),
    );
  });

  let inputs = $state<{ id: string; name: string }[]>([]);
  let testing = $state(false);
  $effect(() => {
    if (step === 3 && inputs.length === 0) {
      // On failure the list stays empty (no reassignment, so this effect doesn't loop);
      // coming back to this step asks again.
      void call<{ inputs: typeof inputs }>('audio.devices')
        .then((r) => (inputs = r.inputs))
        .catch(() => undefined);
    }
    if (step !== 3 && testing) {
      testing = false;
      void meter(false);
    }
  });
  onDestroy(() => {
    if (testing) void meter(false);
  });

  const clip = $derived(s.hotkeys.findIndex((h) => h.action === 'clip.save'));
  const clipBinding = $derived(clip >= 0 ? s.hotkeys[clip] : undefined);

  function setClip(change: { keys?: string; secs?: number }) {
    const list = [...s.hotkeys];
    if (clip >= 0 && list[clip]) list[clip] = { ...list[clip], ...change };
    else list.unshift({ action: 'clip.save', keys: change.keys ?? '', secs: change.secs ?? 30 });
    void save(['hotkeys'], list);
  }

  const finish = () => save(['app', 'first_run_done'], true);
</script>

<div class="backdrop">
  <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="fr-title">
    <header>
      <h2 id="fr-title">Set up Scout</h2>
      <ol class="steps">
        {#each steps as name, i (name)}
          <li class:done={i < step} class:now={i === step}>{name}</li>
        {/each}
      </ol>
    </header>

    <div class="body">
      {#if step === 0}
        <h3>Scout keeps the last moments of your games</h3>
        <p class="muted">
          While a game runs, Scout keeps the last minutes in memory. Press your hotkey and the moment is saved as a
          clip. Nothing is uploaded, and there is no account.
        </p>
        <div class="hw">
          {#if !hw}
            <p class="muted">Checking your graphics card…</p>
          {:else if hw.encoders.length > 0}
            <p>
              <span class="muted">Graphics:</span>
              {hw.gpus.map((g) => g.name).join(', ') || 'unknown'}
            </p>
            <p>
              <span class="muted">Encoder:</span>
              <span class="mono accent">{hw.encoders[0]?.element}</span>
              <span class="muted">({hw.encoders.length} working)</span>
            </p>
          {:else}
            <p class="bad">No working video encoder was found{hw.error ? `: ${hw.error}` : ''}.</p>
          {/if}
        </div>
      {:else if step === 1}
        <h3>How should clips look?</h3>
        <Segmented
          label="Quality preset"
          value={s.recording.quality.preset === 'custom' ? 'balanced' : s.recording.quality.preset}
          options={presets.filter((p) => p.id !== 'custom').map((p) => ({ value: p.id, label: p.label }))}
          onchange={(v) => save(['recording', 'quality', 'preset'], v)}
        />
        <p class="muted">{presets.find((p) => p.id === s.recording.quality.preset)?.detail}</p>
        <div class="line">
          <span class="muted">Keep the last</span>
          <Select
            label="Replay buffer length"
            value={s.recording.buffer_secs}
            options={[30, 60, 120, 300, 600].map((v) => ({ value: v, label: formatDuration(v * 1000) }))}
            onchange={(v) => save(['recording', 'buffer_secs'], v)}
          />
          {#if estimate !== null}
            <span class="muted"
              >· about <span class="mono accent">{formatBytes(estimate)}</span> of memory while a game runs</span
            >
          {/if}
        </div>
      {:else if step === 2}
        <h3>Your clip hotkey</h3>
        <p class="muted">Click the field and press the keys. It works while a game has focus.</p>
        <div class="line">
          <KeyCapture label="Clip hotkey" keys={clipBinding?.keys ?? ''} onchange={(k) => setClip({ keys: k })} />
          <span class="muted">saves the last</span>
          <Select
            label="Clip length"
            value={clipBinding?.secs ?? 30}
            options={[15, 30, 60, 120].map((v) => ({ value: v, label: formatDuration(v * 1000) }))}
            onchange={(v) => setClip({ secs: v })}
          />
        </div>
        {#if fieldError('hotkeys')}<p class="bad">{fieldError('hotkeys')}</p>{/if}
      {:else if step === 3}
        <h3>Microphone</h3>
        <Toggle
          label="Record my microphone in clips"
          checked={s.audio.mic}
          onchange={(v) => save(['audio', 'mic'], v)}
        />
        <div class="line">
          <Select
            label="Microphone"
            value={s.audio.mic_device ?? ''}
            options={[
              { value: '', label: 'Default microphone' },
              ...inputs.map((d) => ({ value: d.id, label: d.name })),
            ]}
            onchange={(v) => save(['audio', 'mic_device'], v || null)}
          />
          <button
            onclick={() => {
              testing = !testing;
              void meter(testing, s.audio.mic_device);
            }}>{testing ? 'Stop test' : 'Test'}</button
          >
        </div>
        {#if testing}<div class="line"><Meter /></div>{/if}
      {:else if step === 4}
        <h3>Where clips go</h3>
        <FolderField
          label="clip folder"
          value={s.clips.folder}
          fallback={app.storage?.folder ?? 'Videos\\Scout'}
          onchange={(v) => save(['clips', 'folder'], v)}
        />
        <Toggle
          label="A folder per game"
          checked={s.clips.per_game_folders}
          onchange={(v) => save(['clips', 'per_game_folders'], v)}
        />
        {#if fieldError('clips')}<p class="bad">{fieldError('clips')}</p>{/if}
      {:else}
        <h3>When a clip is saved</h3>
        <Segmented
          label="Overlay"
          value={s.overlay.mode}
          options={[
            { value: 'off', label: 'No pop-up' },
            { value: 'minimal', label: 'Small pop-up' },
            { value: 'full', label: 'Pop-up and menu' },
          ]}
          onchange={(v) => save(['overlay', 'mode'], v)}
        />
        <Toggle label="Play a sound" checked={s.overlay.sound} onchange={(v) => save(['overlay', 'sound'], v)} />
      {/if}
    </div>

    <footer>
      <button class="flat skip" onclick={finish}>Skip setup</button>
      <span class="grow"></span>
      <button disabled={step === 0} onclick={() => (step -= 1)}>Back</button>
      {#if step < steps.length - 1}
        <button class="primary" onclick={() => (step += 1)}>Next</button>
      {:else}
        <button class="primary" onclick={finish}>Finish</button>
      {/if}
    </footer>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--gutter) 75%, transparent);
    display: grid;
    place-items: center;
    z-index: 20;
  }
  .dialog {
    width: min(760px, calc(100vw - 48px));
    min-height: 420px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 24px 64px rgb(0 0 0 / 45%);
    display: flex;
    flex-direction: column;
  }
  header {
    padding: var(--space-4) var(--space-5) var(--space-3);
    border-bottom: 1px solid var(--border);
  }
  h2 {
    font-size: var(--text-lg);
    color: var(--text-strong);
    margin-bottom: var(--space-3);
  }
  .steps {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    gap: var(--space-4);
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .steps .now {
    color: var(--text-strong);
    text-decoration: underline;
    text-underline-offset: 6px;
  }
  .steps .done {
    color: var(--accent-text);
  }
  .body {
    flex: 1;
    padding: var(--space-5);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    align-items: flex-start;
  }
  h3 {
    font-size: var(--text-xl);
    font-weight: 600;
    color: var(--text-strong);
  }
  .hw {
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: var(--viewer);
    border-radius: var(--radius-sm);
    padding: var(--space-3) var(--space-4);
    min-width: 420px;
  }
  .accent {
    color: var(--accent-text);
  }
  .bad {
    color: var(--bad);
  }
  .line {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
  footer {
    display: flex;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-5);
    border-top: 1px solid var(--border);
  }
  .grow {
    flex: 1;
  }
  .skip {
    color: var(--text-muted);
  }
</style>
