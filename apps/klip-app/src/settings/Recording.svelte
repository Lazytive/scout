<script lang="ts">
  import { call } from '../lib/api';
  import { formatBytes, formatDuration } from '../lib/format';
  import NumberField from '../lib/forms/NumberField.svelte';
  import Row from '../lib/forms/Row.svelte';
  import Segmented from '../lib/forms/Segmented.svelte';
  import Select from '../lib/forms/Select.svelte';
  import Toggle from '../lib/forms/Toggle.svelte';
  import { latestOnly } from '../lib/latest';
  import { bufferChoices, presets, ranges, type Preset, type Settings, type VideoCodec } from '../lib/settings';
  import { fieldError, save } from '../lib/store.svelte';

  let { s }: { s: Settings } = $props();

  const q = $derived(s.recording.quality);
  const c = $derived(q.custom);

  let estimate = $state<{ bytes: number; video_kbps: number; audio_kbps: number; fps: number } | null>(null);
  const takeEstimate = latestOnly();
  $effect(() => {
    const draft = { recording: s.recording, audio: s.audio };
    takeEstimate(
      call<typeof estimate>('recording.estimate', draft),
      (r) => (estimate = r),
      () => (estimate = null),
    );
  });

  let encoders = $state<{ element: string; description: string; gpu: string | null }[]>([]);
  let detecting = $state(true);
  const takeEncoders = latestOnly();
  $effect(() => {
    const codec = q.preset === 'custom' ? (c.codec === 'hevc' ? 'h265' : c.codec) : 'h264';
    detecting = true;
    takeEncoders(
      call<{ encoders: typeof encoders }>('encoders.list', { codec }),
      (r) => {
        encoders = r.encoders;
        detecting = false;
      },
      () => {
        encoders = [];
        detecting = false;
      },
    );
  });

  const bufferOptions = $derived(
    [...new Set([...bufferChoices, s.recording.buffer_secs])]
      .sort((a, b) => a - b)
      .map((v) => ({ value: v, label: formatDuration(v * 1000) })),
  );
</script>

<Row label="Replay buffer" hint="How far back a clip can reach." error={fieldError('recording.buffer_secs')}>
  <Select
    label="Replay buffer length"
    value={s.recording.buffer_secs}
    options={bufferOptions}
    onchange={(v) => save(['recording', 'buffer_secs'], v)}
  />
  {#if estimate}
    <p class="estimate">
      Uses about <strong class="mono">{formatBytes(estimate.bytes)}</strong> of memory while a game runs ({estimate.video_kbps /
        1000} Mbit/s video, {estimate.audio_kbps} kbit/s audio).
    </p>
  {/if}
</Row>

<Row
  label="Quality"
  hint={presets.find((p) => p.id === q.preset)?.detail ?? ''}
  error={fieldError('recording.quality')}
>
  <Segmented
    label="Quality preset"
    value={q.preset}
    options={presets.map((p) => ({ value: p.id, label: p.label }))}
    onchange={(v: Preset) => save(['recording', 'quality', 'preset'], v)}
  />
</Row>

{#if q.preset === 'custom'}
  <Row label="Largest size" hint="Bigger games are scaled down, keeping their shape.">
    <span class="inline">
      <NumberField
        label="Largest width"
        value={c.max_width}
        min={ranges.max_width[0]}
        max={ranges.max_width[1]}
        step={2}
        onchange={(v) => save(['recording', 'quality', 'custom', 'max_width'], v)}
      />
      <span class="muted">×</span>
      <NumberField
        label="Largest height"
        value={c.max_height}
        min={ranges.max_height[0]}
        max={ranges.max_height[1]}
        step={2}
        onchange={(v) => save(['recording', 'quality', 'custom', 'max_height'], v)}
      />
    </span>
  </Row>
  <Row label="Frame rate" error={fieldError('recording.quality.custom.fps')}>
    <NumberField
      label="Frame rate"
      value={c.fps}
      min={ranges.fps[0]}
      max={ranges.fps[1]}
      unit="fps"
      onchange={(v) => save(['recording', 'quality', 'custom', 'fps'], v)}
    />
  </Row>
  <Row label="Bitrate" error={fieldError('recording.quality.custom.bitrate_kbps')}>
    <NumberField
      label="Video bitrate"
      value={c.bitrate_kbps}
      min={ranges.bitrate_kbps[0]}
      max={ranges.bitrate_kbps[1]}
      step={500}
      unit="kbit/s"
      onchange={(v) => save(['recording', 'quality', 'custom', 'bitrate_kbps'], v)}
    />
  </Row>
  <Row label="Codec" hint="H.264 plays everywhere; HEVC and AV1 are smaller but need newer GPUs and players.">
    <Select
      label="Video codec"
      value={c.codec}
      options={[
        { value: 'h264', label: 'H.264' },
        { value: 'hevc', label: 'HEVC (H.265)' },
        { value: 'av1', label: 'AV1' },
      ]}
      onchange={(v: VideoCodec) => save(['recording', 'quality', 'custom', 'codec'], v)}
    />
  </Row>
{/if}

<Row
  label="Encoder"
  hint={detecting ? 'Checking which encoders work on this PC…' : 'Automatic picks the best one that works.'}
>
  <Select
    label="Encoder"
    value={s.recording.encoder ?? ''}
    options={[
      { value: '', label: encoders[0] ? `Automatic (${encoders[0].element})` : 'Automatic' },
      ...encoders.map((e) => ({ value: e.element, label: `${e.element}${e.gpu ? ` · ${e.gpu}` : ''}` })),
    ]}
    onchange={(v) => save(['recording', 'encoder'], v || null)}
  />
</Row>

<Row label="Mouse cursor">
  <Toggle
    label="Show the cursor in clips"
    checked={s.recording.show_cursor}
    onchange={(v) => save(['recording', 'show_cursor'], v)}
  />
</Row>

<style>
  .estimate {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  strong {
    color: var(--accent-text);
    font-weight: 600;
  }
  .inline {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }
</style>
