<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  import { call } from '../lib/api';
  import ListEditor from '../lib/forms/ListEditor.svelte';
  import Meter from '../lib/forms/Meter.svelte';
  import Row from '../lib/forms/Row.svelte';
  import Segmented from '../lib/forms/Segmented.svelte';
  import Select from '../lib/forms/Select.svelte';
  import Toggle from '../lib/forms/Toggle.svelte';
  import { audioBitrates, type Settings } from '../lib/settings';
  import { fieldError, meter, save } from '../lib/store.svelte';

  let { s }: { s: Settings } = $props();
  const a = $derived(s.audio);

  let inputs = $state<{ id: string; name: string }[]>([]);
  let testing = $state(false);

  onMount(() => {
    void call<{ inputs: typeof inputs }>('audio.devices')
      .then((r) => (inputs = r.inputs))
      .catch(() => (inputs = []));
  });
  onDestroy(() => {
    if (testing) void meter(false);
  });

  function test(on: boolean) {
    testing = on;
    void meter(on, a.mic_device);
  }
</script>

<Row
  label="Game sound"
  hint={a.desktop === 'game'
    ? 'Only the game: chat, music and notifications stay out.'
    : a.desktop === 'system'
      ? 'Everything you hear, except the music players left out below.'
      : 'No game sound.'}
>
  <Segmented
    label="Game sound"
    value={a.desktop}
    options={[
      { value: 'game', label: 'Game only' },
      { value: 'system', label: 'Everything' },
      { value: 'off', label: 'Off' },
    ]}
    onchange={(v) => save(['audio', 'desktop'], v)}
  />
</Row>

<Row
  label="Voice chat"
  hint="Voice apps that are running when the game starts get their own track."
  error={fieldError('audio.voice_apps')}
>
  <Toggle label="Record voice chat" checked={a.voice} onchange={(v) => save(['audio', 'voice'], v)} />
  {#if a.voice}
    <ListEditor
      label="Voice apps"
      items={a.voice_apps}
      placeholder="App.exe"
      onchange={(v) => save(['audio', 'voice_apps'], v)}
    />
  {/if}
</Row>

<Row label="Microphone" error={fieldError('audio.mic_device')}>
  <Toggle label="Record my microphone" checked={a.mic} onchange={(v) => save(['audio', 'mic'], v)} />
  <span class="inline">
    <Select
      label="Microphone"
      value={a.mic_device ?? ''}
      options={[{ value: '', label: 'Default microphone' }, ...inputs.map((d) => ({ value: d.id, label: d.name }))]}
      onchange={(v) => {
        void save(['audio', 'mic_device'], v || null);
        if (testing) void meter(true, v || null);
      }}
    />
    <button onclick={() => test(!testing)}>{testing ? 'Stop test' : 'Test'}</button>
  </span>
  {#if testing}
    <span class="inline"><Meter /></span>
  {/if}
</Row>

<Row
  label="Music"
  hint="Keeps copyrighted music out of clips when recording everything."
  error={fieldError('audio.exclude_apps')}
>
  <Toggle
    label="Leave music players out"
    checked={a.default_excludes}
    onchange={(v) => save(['audio', 'default_excludes'], v)}
  />
  <ListEditor
    label="Also leave out"
    items={a.exclude_apps}
    placeholder="App.exe"
    onchange={(v) => save(['audio', 'exclude_apps'], v)}
  />
</Row>

<Row label="Tracks" hint="The first track is always the mix that players play; editors can use the others.">
  <Toggle
    label="Each source on its own track"
    checked={a.separate_tracks}
    onchange={(v) => save(['audio', 'separate_tracks'], v)}
  />
</Row>

<Row label="Audio quality">
  <Select
    label="Audio bitrate"
    value={a.bitrate_kbps}
    options={audioBitrates.map((b) => ({ value: b, label: `${b} kbit/s` }))}
    onchange={(v) => save(['audio', 'bitrate_kbps'], v)}
  />
</Row>

<style>
  .inline {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
  }
</style>
