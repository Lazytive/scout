<!-- The Home workspace: a monitor of the replay buffer, an info panel and the activity /
     hotkey panel, docked like Photocraft / FilmCraft panels. -->
<script lang="ts">
  import { untrack } from 'svelte';
  import { actionLabel, formatBytes, formatDuration, formatTimecode } from './lib/format';
  import Panel from './lib/Panel.svelte';
  import { app, run } from './lib/store.svelte';

  const st = $derived(app.status);
  const rec = $derived(st?.recorder ?? null);
  const live = $derived(!!st?.recording);
  const storage = $derived(app.storage);

  const mode = $derived.by(() => {
    if (!st) return 'connecting';
    if (st.buffer_mode === 'off') return 'off';
    if (st.starting) return 'starting';
    if (st.session) return 'recording';
    if (st.recording) return 'buffering';
    return 'waiting';
  });

  const stateLabel = $derived(
    {
      connecting: 'Connecting',
      off: 'Replay buffer off',
      starting: 'Starting',
      recording: 'Recording',
      buffering: 'Buffering',
      waiting: 'Waiting for a game',
    }[mode],
  );

  const who = $derived(st?.game?.name ?? (st?.desktop ? 'Desktop' : 'No game running'));
  const capacity = $derived(rec?.buffer_ms ?? 0);

  // `status` comes every 2 s (store.svelte.ts) while the buffer grows in real time, so
  // count up locally in between instead of jumping 2 s at a time (human check,
  // 2026-10-10): at most one poll ahead, never past the capacity, and a poll a few ms
  // behind the count doesn't step it back (a bigger drop, e.g. a new game, does).
  const POLL_MS = 2000;
  let polled = $state({ ms: 0, at: 0 });
  let now = $state(0);
  const growing = $derived((mode === 'buffering' || mode === 'recording') && capacity > 0);
  const buffered = $derived(
    growing ? Math.min(capacity, polled.ms + Math.min(POLL_MS, Math.max(0, now - polled.at))) : polled.ms,
  );
  $effect(() => {
    const ms = rec?.buffered_ms ?? 0;
    const at = performance.now();
    const shown = untrack(() => buffered);
    polled = { ms: ms < shown && shown - ms < 500 ? shown : ms, at };
    now = at;
  });
  $effect(() => {
    if (!growing) return;
    const timer = setInterval(() => (now = performance.now()), 200);
    return () => clearInterval(timer);
  });
  const fill = $derived(capacity > 0 ? Math.min(1, buffered / capacity) : 0);

  const clipKeys = $derived(
    (st?.hotkeys ?? []).filter((h) => h.action === 'clip.save' && h.secs),
  );
  const clipLengths = $derived.by(() => {
    const secs = [...new Set(clipKeys.map((h) => h.secs ?? 30))].sort((a, b) => a - b);
    return secs.length > 0 ? secs : [30];
  });

  const source = $derived.by(() => {
    const s = st?.game?.source;
    if (!s) return '–';
    if (s.kind === 'user') return 'Your game list';
    return s.store.charAt(0).toUpperCase() + s.store.slice(1);
  });

  const info = $derived<[string, string][]>([
    ['Game', st?.game?.name ?? (st?.desktop ? 'Desktop' : '–')],
    ['Found in', source],
    ['Encoder', rec?.encoder ?? '–'],
    ['Output', rec?.width && rec.height ? `${rec.width} × ${rec.height}` : '–'],
    ['Frame rate', rec?.fps ? `${rec.fps} fps` : '–'],
    ['Buffer', capacity ? formatDuration(capacity) : '–'],
    ['In memory', rec?.bytes !== undefined ? formatBytes(rec.bytes) : '–'],
    ['Recording', st?.session ? 'On' : 'Off'],
  ]);

  const storageRows = $derived<[string, string][]>(
    storage
      ? [
          ['Clips', `${storage.usage.clips}`],
          ['Size', formatBytes(storage.usage.bytes)],
          ['Starred', `${storage.usage.starred} (${formatBytes(storage.usage.starred_bytes)})`],
          ['Cap', storage.cap_bytes ? formatBytes(storage.cap_bytes) : 'None'],
          ['Over the cap', storage.over_cap === 'trash' ? 'Oldest to Recycle Bin' : 'Oldest deleted'],
          ['Free on drive', storage.free_bytes !== null ? formatBytes(storage.free_bytes) : '–'],
        ]
      : [],
  );
</script>

<div class="workspace">
  <div class="monitor">
    <Panel tabs={['Monitor']}>
        <div class="viewer" data-mode={mode}>
          <div class="state">
            <span class="dot" aria-hidden="true"></span>
            <span>{stateLabel}</span>
          </div>
          <h1>{who}</h1>
          <div class="timecode mono">
            <span class="now">{formatTimecode(buffered)}</span>
            <span class="of">/ {formatTimecode(capacity)}</span>
          </div>
          <div class="ruler" aria-label="Replay buffer filled" role="meter" aria-valuenow={Math.round(fill * 100)} aria-valuemin={0} aria-valuemax={100}>
            <div class="fill" style:width="{fill * 100}%"></div>
            <div class="head" style:left="{fill * 100}%"></div>
          </div>
        </div>
        <div class="transport">
          <button disabled={!live || app.busy} onclick={() => run('session.toggle')}>
            <span class="rec-dot" class:on={st?.session} aria-hidden="true"></span>
            {st?.session ? 'Stop recording' : 'Record'}
          </button>
          <button disabled={!live || app.busy} onclick={() => run('bookmark.add')}>Bookmark</button>
          <span class="grow"></span>
          {#each clipLengths as secs, i (secs)}
            <button
              class:primary={i === 0}
              disabled={!live || app.busy}
              onclick={() => run('clip.save', { last_ms: secs * 1000 })}
            >
              Save last {formatDuration(secs * 1000)}
            </button>
          {/each}
        </div>
    </Panel>
  </div>

  <div class="side">
    <Panel tabs={['Info', 'Storage']}>
      {#snippet children(tab)}
        {#if tab === 'Info'}
          <dl>
            {#each info as [k, v] (k)}
              <dt>{k}</dt>
              <dd class="mono">{v}</dd>
            {/each}
          </dl>
          {#if st?.clip_dir}
            <p class="folder" title={st.clip_dir}>{st.clip_dir}</p>
          {/if}
        {:else if storage}
          <dl>
            {#each storageRows as [k, v] (k)}
              <dt>{k}</dt>
              <dd class="mono">{v}</dd>
            {/each}
          </dl>
          <p class="folder" title={storage.folder}>{storage.folder}</p>
          {#if storage.low_disk}
            <p class="note warn">The clip drive is low on space.</p>
          {/if}
        {:else}
          <p class="muted">No clip folder yet.</p>
        {/if}
      {/snippet}
    </Panel>
  </div>

  <div class="bottom">
    <Panel tabs={['Activity', 'Hotkeys']}>
      {#snippet children(tab)}
        {#if tab === 'Activity'}
          {#if st?.last_error}
            <p class="note bad">Recording couldn't start: {st.last_error}</p>
          {/if}
          {#if app.activity.length === 0}
            <p class="muted">Nothing yet.</p>
          {:else}
            <ul class="log">
              {#each app.activity as a (a.id)}
                <li data-tone={a.tone}>
                  <time class="mono">{a.at.toLocaleTimeString([], { hour12: false })}</time>
                  <span>{a.text}</span>
                </li>
              {/each}
            </ul>
          {/if}
        {:else if !st || st.hotkeys.length === 0}
          <p class="muted">No hotkeys set.</p>
        {:else}
          <ul class="keys">
            {#each st.hotkeys as h (h.id)}
              <li class:broken={!!h.error}>
                <span class="combo">
                  {#each (h.keys ?? '').split('+') as k, i (i)}<kbd>{k}</kbd>{/each}
                </span>
                <span>{actionLabel(h.action, h.secs)}</span>
                {#if h.error}<span class="badge" title={h.error}>{h.error}</span>{/if}
              </li>
            {/each}
          </ul>
        {/if}
      {/snippet}
    </Panel>
  </div>
</div>

<style>
  .workspace {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 300px;
    grid-template-rows: minmax(0, 1fr) 230px;
    gap: 2px;
    height: 100%;
  }
  .monitor,
  .bottom,
  .side {
    display: flex;
    min-height: 0;
    min-width: 0;
  }
  .monitor > :global(*),
  .bottom > :global(*),
  .side > :global(*) {
    flex: 1;
  }
  .side {
    grid-row: 1 / span 2;
    grid-column: 2;
  }
  .viewer {
    background: var(--viewer);
    height: calc(100% - 44px);
    min-height: 180px;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--space-3);
    padding: var(--space-5);
  }
  .state {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    font-size: var(--text-xs);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-muted);
  }
  [data-mode='buffering'] .dot {
    background: var(--good);
  }
  [data-mode='recording'] .dot {
    background: var(--rec);
  }
  [data-mode='starting'] .dot {
    background: var(--warn);
  }
  h1 {
    font-size: var(--text-xl);
    font-weight: 600;
    color: var(--text-strong);
    text-align: center;
  }
  .timecode {
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
  }
  .now {
    font-size: var(--text-timecode);
    color: var(--accent-text);
  }
  .of {
    color: var(--text-muted);
  }
  .ruler {
    width: min(520px, 90%);
    height: 6px;
    background: var(--panel-raised);
    border-radius: 3px;
    position: relative;
  }
  .fill {
    position: absolute;
    inset: 0 auto 0 0;
    background: var(--accent-strong);
    border-radius: 3px;
    transition: width var(--motion);
  }
  .head {
    position: absolute;
    top: -5px;
    width: 2px;
    height: 16px;
    margin-left: -1px;
    background: var(--accent-text);
  }
  .transport {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding-top: var(--space-3);
  }
  .rec-dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    border: 1.5px solid var(--rec);
  }
  .rec-dot.on {
    background: var(--rec);
  }
  .grow {
    flex: 1;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 9px var(--space-4);
    margin: var(--space-2) 0 0;
    font-size: var(--text-sm);
  }
  dt {
    color: var(--text-muted);
  }
  dd {
    margin: 0;
    color: var(--text-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .folder {
    margin-top: var(--space-4);
    font-size: var(--text-xs);
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
  .note {
    margin: var(--space-2) 0;
    font-size: var(--text-sm);
  }
  .note.warn {
    color: var(--warn);
  }
  .note.bad {
    color: var(--bad);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .log li {
    display: flex;
    gap: var(--space-3);
    padding: 3px 0;
    font-size: var(--text-sm);
  }
  .log time {
    color: var(--text-muted);
    white-space: nowrap;
  }
  .log [data-tone='good'] span {
    color: var(--good);
  }
  .log [data-tone='warn'] span {
    color: var(--warn);
  }
  .log [data-tone='bad'] span {
    color: var(--bad);
  }
  .keys li {
    display: grid;
    grid-template-columns: 150px 1fr auto;
    align-items: center;
    gap: var(--space-3);
    padding: 5px 0;
    font-size: var(--text-sm);
  }
  .combo {
    display: inline-flex;
    gap: 3px;
  }
  .broken .combo {
    opacity: 0.45;
  }
  .badge {
    font-size: var(--text-xs);
    color: var(--warn);
  }
</style>
