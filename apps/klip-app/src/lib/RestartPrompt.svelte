<!-- Settings the recorder reads only at its start were changed while it runs: restart
     it now (the replay buffer starts empty) or later (the next start uses them). Shown
     after each such change; the recorder bar keeps a reminder after "Later". -->
<script lang="ts">
  import { formatDuration } from './format';
  import { restartMessage } from './restart';
  import { app, restartLater, restartRecorder } from './store.svelte';

  const st = $derived(app.status);
  const reasons = $derived(st?.restart_pending ?? []);
  const show = $derived(app.restartAsk && reasons.length > 0);
  const buffered = $derived(st?.recorder?.buffered_ms ? formatDuration(st.recorder.buffered_ms) : null);
</script>

{#if show}
  <div class="restart" role="alertdialog" aria-labelledby="restart-title" aria-describedby="restart-text">
    <svg viewBox="0 0 16 16" aria-hidden="true"
      ><path d="M8 1.8l6.5 11.7h-13z" /><path d="M8 6.2v3.6" /><circle class="dot" cx="8" cy="11.6" r="0.8" /></svg
    >
    <div class="text">
      <strong id="restart-title">Restart the recorder to use the new settings?</strong>
      <p id="restart-text">{restartMessage(reasons, buffered, !!st?.session)}</p>
    </div>
    <div class="buttons">
      <button class="primary" disabled={app.busy} onclick={restartRecorder}>Restart now</button>
      <button onclick={restartLater}>Later</button>
    </div>
  </div>
{/if}

<style>
  .restart {
    position: fixed;
    left: 50%;
    bottom: calc(var(--statusbar-h, 0px) + 16px);
    transform: translateX(-50%);
    z-index: 51;
    width: min(620px, calc(100vw - 32px));
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: var(--space-2) var(--space-3);
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--border-strong);
    border-left: 3px solid var(--warn);
    border-radius: var(--radius);
    background: var(--panel-raised);
    color: var(--text-strong);
    box-shadow: 0 6px 24px rgb(0 0 0 / 35%);
    animation: rise var(--motion, 160ms) ease-out;
  }
  svg {
    width: 18px;
    height: 18px;
    margin-top: 1px;
    fill: none;
    stroke: var(--warn);
    stroke-width: 1.3;
    stroke-linejoin: round;
  }
  .dot {
    fill: var(--warn);
    stroke: none;
  }
  strong {
    font-size: var(--text-md);
    font-weight: 600;
  }
  p {
    margin: 4px 0 0;
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .buttons {
    grid-column: 2;
    display: flex;
    gap: var(--space-2);
  }
  @keyframes rise {
    from {
      opacity: 0;
      transform: translate(-50%, 8px);
    }
  }
</style>
