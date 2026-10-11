<script lang="ts">
  import { formatBytes } from '../lib/format';
  import FolderField from '../lib/forms/FolderField.svelte';
  import NumberField from '../lib/forms/NumberField.svelte';
  import Row from '../lib/forms/Row.svelte';
  import Segmented from '../lib/forms/Segmented.svelte';
  import Toggle from '../lib/forms/Toggle.svelte';
  import { ranges, type Settings } from '../lib/settings';
  import { app, fieldError, save } from '../lib/store.svelte';

  let { s }: { s: Settings } = $props();
  const st = $derived(app.storage);
</script>

<Row label="Clip folder" hint="Clips are plain MP4 files: open, move or back them up like any file." error={fieldError('clips.folder')}>
  <FolderField
    label="clip folder"
    value={s.clips.folder}
    fallback={st?.folder ?? 'Videos\\Scout'}
    onchange={(v) => save(['clips', 'folder'], v)}
  />
  <Toggle
    label="A folder per game"
    checked={s.clips.per_game_folders}
    onchange={(v) => save(['clips', 'per_game_folders'], v)}
  />
</Row>

<Row label="Recordings folder" hint="Full-session recordings (Record button or hotkey)." error={fieldError('clips.recordings_folder')}>
  <FolderField
    label="recordings folder"
    value={s.clips.recordings_folder}
    fallback="Same as the clip folder"
    onchange={(v) => save(['clips', 'recordings_folder'], v)}
  />
</Row>

<Row
  label="Size limit"
  hint={s.storage.cap_gb === 0
    ? 'No limit.'
    : 'When the clip folder grows past this, the oldest clips go first. Starred clips are never removed.'}
  error={fieldError('storage.cap_gb')}
>
  <NumberField
    label="Size limit"
    value={s.storage.cap_gb}
    min={ranges.cap_gb[0]}
    max={ranges.cap_gb[1]}
    unit="GB (0: no limit)"
    onchange={(v) => save(['storage', 'cap_gb'], v)}
  />
  <Segmented
    label="Old clips"
    value={s.storage.over_cap}
    options={[
      { value: 'trash', label: 'Move to the Recycle Bin' },
      { value: 'delete', label: 'Delete' },
    ]}
    onchange={(v) => save(['storage', 'over_cap'], v)}
  />
</Row>

<Row label="Low disk warning" error={fieldError('storage.low_disk_gb')}>
  <NumberField
    label="Warn below"
    value={s.storage.low_disk_gb}
    min={ranges.low_disk_gb[0]}
    max={ranges.low_disk_gb[1]}
    unit="GB free (0: never)"
    onchange={(v) => save(['storage', 'low_disk_gb'], v)}
  />
</Row>

{#if st}
  <Row label="Now">
    <dl>
      <dt>Clips</dt>
      <dd class="mono">{st.usage.clips} · {formatBytes(st.usage.bytes)}</dd>
      <dt>Starred</dt>
      <dd class="mono">{st.usage.starred} · {formatBytes(st.usage.starred_bytes)}</dd>
      <dt>Free on the drive</dt>
      <dd class="mono" class:warn={st.low_disk}>{st.free_bytes === null ? '–' : formatBytes(st.free_bytes)}</dd>
    </dl>
  </Row>
{/if}

<style>
  dl {
    display: grid;
    grid-template-columns: 140px auto;
    gap: 6px var(--space-3);
    margin: 4px 0 0;
  }
  dt {
    color: var(--text-muted);
  }
  dd {
    margin: 0;
    color: var(--text-strong);
  }
  .warn {
    color: var(--warn);
  }
</style>
