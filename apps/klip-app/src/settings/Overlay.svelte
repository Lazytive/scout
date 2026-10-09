<script lang="ts">
  import { call } from '../lib/api';
  import NumberField from '../lib/forms/NumberField.svelte';
  import Row from '../lib/forms/Row.svelte';
  import Segmented from '../lib/forms/Segmented.svelte';
  import Select from '../lib/forms/Select.svelte';
  import Toggle from '../lib/forms/Toggle.svelte';
  import { ranges, type Settings } from '../lib/settings';
  import { fieldError, save } from '../lib/store.svelte';

  let { s }: { s: Settings } = $props();
  const o = $derived(s.overlay);
</script>

<Row
  label="When a clip is saved"
  hint={o.mode === 'off'
    ? 'Nothing on screen; a sound and/or a notification if you turn them on below.'
    : o.mode === 'minimal'
      ? 'A small “Clip saved” card with a picture, for a few seconds. It never takes focus from the game.'
      : 'The card, a dot while recording (a window always on top of the game: it can keep G-Sync/FreeSync off while shown), and a quick menu on its hotkey.'}
>
  <Segmented
    label="Overlay"
    value={o.mode}
    options={[
      { value: 'off', label: 'Off' },
      { value: 'minimal', label: 'Minimal' },
      { value: 'full', label: 'Full' },
    ]}
    onchange={(v) => save(['overlay', 'mode'], v)}
  />
</Row>

{#if o.mode !== 'off'}
  <Row label="Position">
    <Select
      label="Overlay corner"
      value={o.corner}
      options={[
        { value: 'top-left', label: 'Top left' },
        { value: 'top-right', label: 'Top right' },
        { value: 'bottom-left', label: 'Bottom left' },
        { value: 'bottom-right', label: 'Bottom right' },
      ]}
      onchange={(v) => save(['overlay', 'corner'], v)}
    />
  </Row>
  <Row label="Shown for" error={fieldError('overlay.seconds')}>
    <NumberField
      label="Seconds on screen"
      value={o.seconds}
      min={ranges.overlay_seconds[0]}
      max={ranges.overlay_seconds[1]}
      unit="s"
      onchange={(v) => save(['overlay', 'seconds'], v)}
    />
  </Row>
{/if}

<Row label="Preview" hint="Shows the pop-up as it will look over a game.">
  <button onclick={() => call('overlay.preview').catch(() => undefined)}>Show me</button>
</Row>

<Row label="Also">
  <Toggle label="Play a sound" checked={o.sound} onchange={(v) => save(['overlay', 'sound'], v)} />
  <Toggle
    label="Show a system notification"
    checked={o.notification}
    onchange={(v) => save(['overlay', 'notification'], v)}
  />
  {#if o.mode === 'full'}
    <Toggle
      label="Recording dot while recording"
      checked={o.recording_dot}
      onchange={(v) => save(['overlay', 'recording_dot'], v)}
    />
  {/if}
</Row>
