<script lang="ts">
  import Row from '../lib/forms/Row.svelte';
  import Toggle from '../lib/forms/Toggle.svelte';
  import type { Settings } from '../lib/settings';
  import { app, checkForUpdates, installUpdate, openReleases, save } from '../lib/store.svelte';
  import { updateLine } from '../lib/update';

  let { s }: { s: Settings } = $props();

  const busy = $derived(
    app.update?.state === 'checking' || app.update?.state === 'downloading' || app.update?.state === 'installing',
  );
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

<Row
  label="Updates"
  hint="Once a day Scout asks github.com for a newer release: one request, with nothing about you in it. An update installs only when you say so."
>
  <Toggle label="Check for updates" checked={s.app.update_check} onchange={(v) => save(['app', 'update_check'], v)} />
  <div class="update">
    <span class="muted">{updateLine(app.update, Date.now())}</span>
    <span class="buttons">
      <button onclick={() => void checkForUpdates()} disabled={busy}>
        {app.update?.state === 'checking' ? 'Checking…' : 'Check now'}
      </button>
      {#if app.update?.state === 'available'}
        {#if app.update.installs}
          <button class="primary" onclick={() => void installUpdate()}>Install {app.update.version} and restart</button>
        {:else}
          <button class="primary" onclick={() => void openReleases()}>Get {app.update.version} on GitHub</button>
        {/if}
      {/if}
    </span>
  </div>
</Row>

<Row label="Setup">
  <button onclick={() => save(['app', 'first_run_done'], false)}>Run the first-time setup again</button>
</Row>

<style>
  .update {
    display: grid;
    gap: var(--space-2);
  }
  .muted {
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .buttons {
    display: flex;
    gap: var(--space-2);
    flex-wrap: wrap;
  }
</style>
