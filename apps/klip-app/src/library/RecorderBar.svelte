<!-- The recorder in the app's title bar (M11.2 the old Home, in the title bar since
     M11.3): the replay buffer's state, game and fill, Record / Save (Bookmark and the
     other lengths on wide windows), a problem badge, and a pop-up with every action, the
     recorder's details, problems and the activity log. Narrow windows drop pieces in
     order; the pop-up keeps them all. -->
<script lang="ts">
  import { untrack } from 'svelte';

  import { actionLabel, formatBytes, formatDuration, formatTimecode } from '../lib/format';
  import { app, run } from '../lib/store.svelte';

  const st = $derived(app.status);
  const rec = $derived(st?.recorder ?? null);
  const live = $derived(!!st?.recording);

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

  const who = $derived(st?.game?.name ?? (st?.desktop ? 'Desktop' : null));
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

  const clipLengths = $derived.by(() => {
    const keys = (st?.hotkeys ?? []).filter((h) => h.action === 'clip.save' && h.secs);
    const secs = [...new Set(keys.map((h) => h.secs ?? 30))].sort((a, b) => a - b);
    return secs.length > 0 ? secs : [30];
  });

  const brokenKeys = $derived((st?.hotkeys ?? []).filter((h) => !!h.error));
  const problems = $derived((st?.last_error ? 1 : 0) + brokenKeys.length);

  const source = $derived.by(() => {
    const s = st?.game?.source;
    if (!s) return '–';
    if (s.kind === 'user') return 'Your game list';
    return s.store.charAt(0).toUpperCase() + s.store.slice(1);
  });

  const info = $derived<[string, string][]>([
    ['Game', who ?? '–'],
    ['Found in', source],
    ['Encoder', rec?.encoder ?? '–'],
    ['Output', rec?.width && rec.height ? `${rec.width} × ${rec.height}` : '–'],
    ['Frame rate', rec?.fps ? `${rec.fps} fps` : '–'],
    ['Buffer', capacity ? formatDuration(capacity) : '–'],
    ['In memory', rec?.bytes !== undefined ? formatBytes(rec.bytes) : '–'],
    ['Recording', st?.session ? 'On' : 'Off'],
  ]);

  let open = $state(false);
  let root: HTMLElement | undefined = $state();

  function outside(e: PointerEvent) {
    if (open && root && !root.contains(e.target as Node)) open = false;
  }
</script>

<svelte:window
  onpointerdown={outside}
  onkeydown={(e) => {
    if (open && e.key === 'Escape') open = false;
  }}
/>

<div class="bar" data-mode={mode} bind:this={root}>
  <span class="state" title={stateLabel}><span class="dot" aria-hidden="true"></span><span class="label">{stateLabel}</span></span>
  {#if who}<span class="who" title={who}>{who}</span>{/if}
  <div class="meter" title="Replay buffer: {formatTimecode(buffered)} of {formatTimecode(capacity)}">
    <span class="mono now">{formatTimecode(buffered)}</span>
    <div class="ruler" aria-label="Replay buffer filled" role="meter" aria-valuenow={Math.round(fill * 100)} aria-valuemin={0} aria-valuemax={100}>
      <div class="fill" style:width="{fill * 100}%"></div>
    </div>
    <span class="mono of">{formatTimecode(capacity)}</span>
  </div>
  {#if problems > 0}
    {@const what = st?.last_error ? 'Recording couldn’t start' : `${brokenKeys.length} hotkey${brokenKeys.length === 1 ? '' : 's'} not working`}
    <button class="flat problem" onclick={() => (open = true)} title="{what}: show what went wrong" aria-label={what}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 1.8l6.5 11.7h-13z" /><path d="M8 6.2v3.6" /><circle class="fill-dot" cx="8" cy="11.6" r="0.8" /></svg>
      <span class="label">{what}</span>
    </button>
  {/if}
  <span class="grow"></span>
  <button class="rec" disabled={!live || app.busy} onclick={() => run('session.toggle')}>
    <span class="rec-dot" class:on={st?.session} aria-hidden="true"></span>
    {st?.session ? 'Stop recording' : 'Record'}
  </button>
  <button class="extra" disabled={!live || app.busy} onclick={() => run('bookmark.add')}>Bookmark</button>
  {#each clipLengths as secs, i (secs)}
    <button class:primary={i === 0} class:extra={i > 0} disabled={!live || app.busy} onclick={() => run('clip.save', { last_ms: secs * 1000 })}>
      Save last {formatDuration(secs * 1000)}
    </button>
  {/each}
  <button
    class="flat more"
    class:on={open}
    aria-expanded={open}
    aria-label="Recorder details and activity"
    title="Recorder details and activity"
    onclick={() => (open = !open)}
  >
    <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="6.5" /><path d="M8 7v4.5" /><circle class="fill-dot" cx="8" cy="4.8" r="0.9" /></svg>
  </button>

  {#if open}
    <div class="pop" role="dialog" aria-label="Recorder details and activity">
      <div class="actions">
        <button disabled={!live || app.busy} onclick={() => run('session.toggle')}>
          <span class="rec-dot" class:on={st?.session} aria-hidden="true"></span>
          {st?.session ? 'Stop recording' : 'Record'}
        </button>
        <button disabled={!live || app.busy} onclick={() => run('bookmark.add')}>Bookmark</button>
        {#each clipLengths as secs (secs)}
          <button disabled={!live || app.busy} onclick={() => run('clip.save', { last_ms: secs * 1000 })}>Save last {formatDuration(secs * 1000)}</button>
        {/each}
      </div>
      <section>
        <h3>Recorder</h3>
        <dl>
          {#each info as [k, v] (k)}
            <dt>{k}</dt>
            <dd class="mono">{v}</dd>
          {/each}
        </dl>
        {#if st?.clip_dir}<p class="folder" title={st.clip_dir}>{st.clip_dir}</p>{/if}
      </section>
      <section>
        <h3>Activity</h3>
        {#if st?.last_error}<p class="note bad">Recording couldn’t start: {st.last_error}</p>{/if}
        {#each brokenKeys as h (h.id)}
          <p class="note warn">
            <span class="combo">{#each (h.keys ?? '').split('+') as k, i (i)}<kbd>{k}</kbd>{/each}</span>
            ({actionLabel(h.action, h.secs)}): {h.error}.
            <button class="link" onclick={() => ((open = false), (app.settingsPage = 'hotkeys'), (app.page = 'settings'))}>Hotkey settings</button>
          </p>
        {/each}
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
      </section>
    </div>
  {/if}
</div>

<style>
  .bar {
    position: relative;
    flex: 1;
    align-self: stretch;
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    padding-left: var(--space-3);
    border-left: 1px solid var(--border);
    margin: 8px 0;
  }
  .state {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex: none;
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
  .who {
    color: var(--text-strong);
    font-weight: 600;
    min-width: 0;
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meter {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    margin-left: var(--space-2);
  }
  .now {
    color: var(--accent-text);
  }
  .of {
    color: var(--text-muted);
  }
  .ruler {
    position: relative;
    width: 140px;
    min-width: 40px;
    flex: 0 1 140px;
    height: 4px;
    background: var(--panel-raised);
    border-radius: 2px;
  }
  .fill {
    position: absolute;
    inset: 0 auto 0 0;
    background: var(--accent-strong);
    border-radius: 2px;
    transition: width var(--motion);
  }
  .problem {
    flex: none;
    gap: 5px;
    color: var(--warn);
    font-size: var(--text-sm);
  }
  .problem svg {
    width: 15px;
    height: 15px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
    stroke-linejoin: round;
  }
  .problem .fill-dot {
    fill: currentColor;
    stroke: none;
  }
  .grow {
    flex: 1;
  }
  .bar > button {
    flex: none;
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
  .more {
    width: 28px;
    padding: 0;
    justify-content: center;
  }
  .more svg {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
    stroke-linecap: round;
  }
  .more .fill-dot {
    fill: currentColor;
    stroke: none;
  }
  .more.on {
    color: var(--accent-text);
  }
  /* Narrow windows drop pieces in order (all of them stay in the pop-up): the extra
     buttons, the capacity and the problem's words; the ruler; the state's words and the
     time; Record. Scout opens at 1180 px, its smallest is 820 px. */
  @media (max-width: 1499px) {
    .bar > .extra,
    .of,
    .problem .label {
      display: none;
    }
  }
  @media (max-width: 1249px) {
    .ruler {
      display: none;
    }
  }
  @media (max-width: 999px) {
    .state .label,
    .now {
      display: none;
    }
    .who {
      max-width: 110px;
    }
  }
  @media (max-width: 899px) {
    .bar > .rec {
      display: none;
    }
  }
  .pop {
    position: absolute;
    top: calc(100% + 10px);
    right: var(--space-2);
    z-index: 20;
    width: min(640px, calc(100vw - 32px));
    max-height: 60vh;
    overflow: auto;
    display: grid;
    grid-template-columns: 240px minmax(0, 1fr);
    gap: var(--space-3) var(--space-4);
    padding: var(--space-3) var(--space-4);
    background: var(--panel-raised);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius);
    box-shadow: 0 8px 24px rgb(0 0 0 / 35%);
  }
  .actions {
    grid-column: 1 / -1;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    padding-bottom: var(--space-3);
    border-bottom: 1px solid var(--border);
  }
  h3 {
    margin: 0 0 var(--space-2);
    font-size: var(--text-xs);
    font-weight: 600;
    color: var(--text-muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px var(--space-3);
    margin: 0;
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
    margin-top: var(--space-3);
    font-size: var(--text-xs);
    color: var(--text-muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
  .note {
    margin: 0 0 var(--space-2);
    font-size: var(--text-sm);
  }
  .note.warn {
    color: var(--warn);
  }
  .note.bad {
    color: var(--bad);
  }
  .combo {
    display: inline-flex;
    gap: 3px;
  }
  .link {
    background: none;
    border: none;
    padding: 0;
    height: auto;
    min-height: 0;
    color: var(--accent-text);
    text-decoration: underline;
    cursor: pointer;
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
</style>
