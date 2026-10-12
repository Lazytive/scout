<!-- Settings → Debug (tester builds only, `full-logs`): the self-test (lib/selftest.ts),
     its report beside the logs, and the logs folder. -->
<script lang="ts">
  import { onMount } from 'svelte';

  import { call, debugInfo, onEvent, openLogs, saveDebugReport, type DebugInfo } from '../lib/api';
  import type { Settings } from '../lib/settings';
  import { runSelfTest, type Step } from '../lib/selftest';
  import type { KlipEvent } from '../lib/types';

  let { s }: { s: Settings } = $props();

  let info = $state<DebugInfo | null>(null);
  let steps = $state<Step[]>([]);
  let running = $state(false);
  let report = $state('');
  let saved = $state<string | null>(null);
  let error = $state<string | null>(null);
  let copied = $state(false);

  onMount(() => {
    void debugInfo().then((i) => (info = i));
  });

  async function run() {
    if (running || !info) return;
    running = true;
    saved = null;
    error = null;
    report = '';
    type Waiter = { pred: (e: KlipEvent) => boolean; done: (e: KlipEvent | null) => void };
    const waiters: Waiter[] = [];
    const drop = (w: Waiter) => {
      const i = waiters.indexOf(w);
      if (i >= 0) waiters.splice(i, 1);
      return i >= 0;
    };
    const stop = await onEvent((e) => {
      for (const w of [...waiters]) {
        if (w.pred(e) && drop(w)) w.done(e);
      }
    });
    try {
      const r = await runSelfTest(
        {
          call,
          waitFor: (pred, ms) =>
            new Promise((resolve) => {
              const w: Waiter = { pred, done: resolve };
              waiters.push(w);
              setTimeout(() => {
                if (drop(w)) resolve(null);
              }, ms);
            }),
          sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
          now: () => Date.now(),
          info,
        },
        (st) => (steps = st),
      );
      steps = r.steps;
      report = r.report;
      saved = await saveDebugReport(r.report);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      stop();
      running = false;
    }
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(report);
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch {
      copied = false;
    }
  }
</script>

<p class="lead">
  For testers. The self-test probes every encoder again, records a few seconds with each working H.264 encoder (your
  screen, or the game if one is running), makes thumbnails and a preview frame, exports with the YouTube and Discord
  presets, then deletes the test clips and puts your settings back. It takes about two minutes; leave Scout open until
  it says it's done.
</p>
{#if info}
  <p class="muted">
    Scout {info.version} · full logs {info.full_logs ? 'on' : 'off'} · bundled GStreamer {info.bundled_gstreamer
      ? 'yes'
      : 'no'} · replay buffer {s.games.buffer} · encoder {s.recording.encoder ?? 'automatic'}
  </p>
{/if}

<div class="actions">
  <button class="primary" onclick={() => void run()} disabled={running || !info}>
    {running ? 'Testing…' : 'Run the self-test'}
  </button>
  <button onclick={() => void openLogs()}>Open the logs folder</button>
  {#if report}
    <button onclick={() => void copy()}>{copied ? 'Copied' : 'Copy the report'}</button>
  {/if}
</div>

{#if error}<p class="error" role="alert">{error}</p>{/if}
{#if saved}
  <p class="done">
    Done. The report is saved as <span class="mono">{saved}</span> in the logs folder: send the whole folder.
  </p>
{/if}

{#if steps.length > 0}
  <ol class="steps">
    {#each steps as st (st.id)}
      <li class={st.status}>
        <span class="mark" aria-label={st.status}>
          {st.status === 'ok'
            ? '✓'
            : st.status === 'fail'
              ? '✗'
              : st.status === 'warn'
                ? '!'
                : st.status === 'skip'
                  ? '–'
                  : st.status === 'running'
                    ? '…'
                    : '·'}
        </span>
        <span class="label">{st.label}</span>
        {#if st.detail}<pre class="detail">{st.detail}</pre>{/if}
      </li>
    {/each}
  </ol>
{/if}

<style>
  .lead {
    max-width: 70ch;
    margin: 0 0 8px;
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
    margin: 12px 0;
  }
  .steps {
    list-style: none;
    padding: 0;
    margin: 8px 0;
    display: grid;
    gap: 6px;
  }
  .steps li {
    display: grid;
    grid-template-columns: 20px 1fr;
    gap: 2px 8px;
    align-items: baseline;
  }
  .mark {
    font-weight: 700;
    text-align: center;
  }
  .ok .mark {
    color: var(--good);
  }
  .fail .mark {
    color: var(--bad);
  }
  .warn .mark {
    color: var(--warn);
  }
  .skip,
  .pending {
    color: var(--text-muted);
  }
  .detail {
    grid-column: 2;
    margin: 0;
    white-space: pre-wrap;
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .done {
    color: var(--good);
  }
  .error {
    color: var(--bad);
  }
</style>
