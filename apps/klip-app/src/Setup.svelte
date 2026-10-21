<!-- Scout's setup window (ADR 0023): the app's own look around the installer inside the
     setup app. Ready → installing (a progress bar) → done (Open Scout) or failed (Try
     again). With `/P` (the updater) it installs by itself and opens Scout when told to. -->
<script lang="ts">
  import { onMount } from 'svelte';

  import {
    primaryLabel,
    secondaryLabel,
    setupBackend,
    setupLine,
    setupTitle,
    type SetupInfo,
    type SetupPhase,
  } from './lib/setup';
  import { closeWindow, customChrome, dragsWindow, minimize, startDrag } from './lib/window-chrome';

  const backend = setupBackend();
  let info = $state<SetupInfo | null>(null);
  let phase = $state<SetupPhase>('ready');
  let pct = $state(0);
  let error = $state<string | null>(null);

  async function install() {
    if (phase === 'installing') return;
    phase = 'installing';
    pct = 0;
    error = null;
    try {
      await backend.install((p) => (pct = p));
      pct = 100;
      phase = 'done';
      if (info?.passive && info.run_after) await backend.open();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      phase = 'failed';
    }
  }

  function primary() {
    if (phase === 'done') void backend.open();
    else void install();
  }

  function bar(e: MouseEvent) {
    if (e.button === 0 && dragsWindow(e.target as Element | null)) void startDrag();
  }

  onMount(() => {
    void backend.info().then((i) => {
      info = i;
      if (i.passive && i.has_installer) void install();
    });
  });
</script>

<div class="setup">
  {#if customChrome}
    <div class="bar" role="presentation" onmousedown={bar}>
      <span class="name">Scout Setup</span>
      <span class="grow"></span>
      <button class="flat cap" aria-label="Minimize" title="Minimize" onclick={minimize}>
        <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0 5.5h10" /></svg>
      </button>
      <button class="flat cap close" aria-label="Close" title="Close" onclick={closeWindow}>
        <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0.5 0.5l9 9M9.5 0.5l-9 9" /></svg>
      </button>
    </div>
  {/if}
  <main>
    <svg class="icon" viewBox="0 0 1024 1024" aria-hidden="true">
      <rect x="64" y="64" width="896" height="896" rx="224" fill="#3490f7" />
      <path d="M392 300 L744 512 L392 724 Z" fill="#fff" stroke="#fff" stroke-width="48" stroke-linejoin="round" />
    </svg>
    <h1>{setupTitle(phase, info)}</h1>
    <p class="line" class:error={phase === 'failed'}>{setupLine(phase, info, pct, error)}</p>
    {#if phase === 'installing'}
      <div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax="100" aria-valuenow={pct}>
        <span style:width="{pct}%"></span>
      </div>
    {/if}
    <div class="buttons">
      <button
        class="primary"
        disabled={phase === 'installing' || (phase === 'ready' && !info?.has_installer)}
        onclick={primary}
      >
        {primaryLabel(phase)}
      </button>
      {#if secondaryLabel(phase)}
        <button onclick={() => void backend.quit()}>{secondaryLabel(phase)}</button>
      {/if}
    </div>
  </main>
  <footer>Scout {info?.version ?? ''} · MIT OR Apache-2.0 · no account, no telemetry</footer>
</div>

<style>
  .setup {
    height: 100vh;
    display: grid;
    grid-template-rows: auto 1fr auto;
    background: var(--panel);
    color: var(--text);
    user-select: none;
  }
  .bar {
    height: var(--header-h);
    display: flex;
    align-items: center;
    padding-left: var(--space-4);
    background: var(--gutter);
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .grow {
    flex: 1;
  }
  .cap {
    width: 46px;
    height: 100%;
    min-height: 0;
    padding: 0;
    border: none;
    border-radius: 0;
    display: grid;
    place-items: center;
    color: var(--text);
  }
  .cap svg {
    width: 10px;
    height: 10px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1;
  }
  .cap:hover {
    background: var(--hover);
  }
  .cap.close:hover {
    background: #c42b1c;
    color: #fff;
  }
  main {
    display: grid;
    justify-items: center;
    align-content: center;
    gap: var(--space-3);
    padding: var(--space-6) var(--space-6) var(--space-4);
    text-align: center;
  }
  .icon {
    width: 96px;
    height: 96px;
    margin-bottom: var(--space-2);
  }
  h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 600;
    color: var(--text-strong);
  }
  .line {
    margin: 0;
    max-width: 44ch;
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .line.error {
    color: var(--bad);
  }
  .progress {
    width: 320px;
    height: 4px;
    border-radius: 2px;
    background: var(--border);
    overflow: hidden;
  }
  .progress span {
    display: block;
    height: 100%;
    background: var(--accent);
    transition: width var(--motion) linear;
  }
  .buttons {
    display: flex;
    gap: var(--space-2);
    margin-top: var(--space-3);
  }
  footer {
    padding: var(--space-2) var(--space-4) var(--space-3);
    color: var(--text-muted);
    font-size: var(--text-sm);
    text-align: center;
  }
</style>
