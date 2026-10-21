<!-- A newer Scout is on GitHub (ADR 0022): its version and notes, "Install and restart"
     (Windows: the signed installer is fetched, verified and run; Scout exits for it) or
     "Get it on GitHub", and "Later" (the title bar keeps an Update button). -->
<script lang="ts">
  import { app, installUpdate, openReleases, updateLater } from './store.svelte';
  import { updatePending, updateTitle } from './update';

  const u = $derived(app.update);
  const show = $derived(app.updateAsk && updatePending(u));
  const busy = $derived(u?.state === 'downloading' || u?.state === 'installing');
</script>

{#if show && u}
  <div class="update" role="alertdialog" aria-labelledby="update-title" aria-describedby="update-text">
    <svg viewBox="0 0 16 16" aria-hidden="true"
      ><path d="M8 2v8" /><path d="M4.5 6.5 8 10l3.5-3.5" /><path d="M3 13h10" /></svg
    >
    <div class="text">
      <strong id="update-title">{updateTitle(u)}</strong>
      {#if u.notes}
        <pre id="update-text">{u.notes}</pre>
      {:else}
        <p id="update-text">What changed is on the release page.</p>
      {/if}
      {#if u.state === 'downloading'}
        <div class="bar" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={u.progress ?? 0}>
          <span style:width="{u.progress ?? 0}%"></span>
        </div>
      {/if}
      {#if u.error}<p class="error" role="alert">{u.error}</p>{/if}
    </div>
    <div class="buttons">
      {#if u.installs}
        <button class="primary" disabled={busy} onclick={() => void installUpdate()}>
          {u.state === 'installing'
            ? 'Installing…'
            : u.state === 'downloading'
              ? 'Downloading…'
              : 'Install and restart'}
        </button>
      {:else}
        <button class="primary" onclick={() => void openReleases()}>Get it on GitHub</button>
      {/if}
      <button disabled={busy} onclick={updateLater}>Later</button>
    </div>
  </div>
{/if}

<style>
  .update {
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
    border-left: 3px solid var(--accent);
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
    stroke: var(--accent);
    stroke-width: 1.3;
    stroke-linecap: round;
    stroke-linejoin: round;
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
  pre {
    margin: 4px 0 0;
    max-height: 160px;
    overflow: auto;
    white-space: pre-wrap;
    font: inherit;
    font-size: var(--text-sm);
    color: var(--text-muted);
  }
  .bar {
    margin-top: var(--space-2);
    height: 4px;
    border-radius: 2px;
    background: var(--border);
    overflow: hidden;
  }
  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width var(--motion) linear;
  }
  .error {
    color: var(--bad);
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
