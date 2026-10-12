<script lang="ts">
  import Row from '../lib/forms/Row.svelte';
  import Toggle from '../lib/forms/Toggle.svelte';
  import type { Settings } from '../lib/settings';
  import { save } from '../lib/store.svelte';

  let { s }: { s: Settings } = $props();
</script>

<Row label="Start">
  <Toggle label="Start Scout with Windows" checked={s.app.autostart} onchange={(v) => save(['app', 'autostart'], v)} />
  <Toggle
    label="Open in the tray (no window)"
    checked={s.app.start_minimized}
    onchange={(v) => save(['app', 'start_minimized'], v)}
  />
</Row>

<Row label="Closing the window" hint="Scout keeps recording games from the tray.">
  <Toggle
    label="Keep Scout running in the tray"
    checked={s.app.close_to_tray}
    onchange={(v) => save(['app', 'close_to_tray'], v)}
  />
</Row>

<Row
  label="Control from scripts"
  hint="Lets klip-cli and your own scripts on this PC save clips (for a desktop shortcut, a Stream Deck…). Nothing outside this PC can connect. Takes effect when Scout starts."
>
  <Toggle
    label="Allow local control"
    checked={s.app.control_channel}
    onchange={(v) => save(['app', 'control_channel'], v)}
  />
</Row>

<Row label="Setup">
  <button onclick={() => save(['app', 'first_run_done'], false)}>Run the first-time setup again</button>
</Row>
