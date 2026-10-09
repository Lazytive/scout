<script lang="ts">
  import { onMount } from 'svelte';

  import FirstRun from './FirstRun.svelte';
  import Home from './Home.svelte';
  import Library from './Library.svelte';
  import { formatDuration } from './lib/format';
  import ThemeMenu from './lib/ThemeMenu.svelte';
  import Settings from './Settings.svelte';
  import { app, connect } from './lib/store.svelte';

  type Page = 'home' | 'library' | 'settings';
  // KLIP_START_PAGE (tests): the app injects the page to open on.
  const start = (globalThis as { __KLIP_START_PAGE__?: string }).__KLIP_START_PAGE__;
  let page = $state<Page>(start === 'library' || start === 'settings' ? start : 'home');

  const workspaces: { id: Page; label: string; soon?: boolean }[] = [
    { id: 'home', label: 'Home' },
    { id: 'library', label: 'Library' },
    { id: 'settings', label: 'Settings' },
  ];

  const st = $derived(app.status);
  const title = $derived.by(() => {
    if (!st) return 'Klip';
    const who = st.game?.name ?? (st.desktop ? 'Desktop' : null);
    if (!who) return st.buffer_mode === 'off' ? 'Replay buffer off' : 'Waiting for a game';
    if (st.starting) return `${who} — starting`;
    return `${who} — ${st.session ? 'recording' : 'buffering'}`;
  });

  const hint = $derived.by(() => {
    const clip = st?.hotkeys.find((h) => h.action === 'clip.save' && !h.error);
    if (!st?.recording) return 'Klip starts the replay buffer by itself when a game opens.';
    if (clip?.keys && clip.secs) return `${clip.keys} saves the last ${formatDuration(clip.secs * 1000)}.`;
    return 'Save a clip with the button or a hotkey.';
  });

  onMount(() => {
    let stop: (() => void) | undefined;
    void connect().then((s) => (stop = s));
    return () => stop?.();
  });
</script>

<div class="shell">
  <header class="appbar">
    <div class="left">
      <span class="logo" aria-hidden="true"></span>
      <nav aria-label="Workspaces">
        {#each workspaces as w (w.id)}
          <button
            class="flat ws"
            class:active={page === w.id}
            disabled={w.soon}
            title={w.soon ? 'Coming soon' : undefined}
            aria-current={page === w.id ? 'page' : undefined}
            onclick={() => (page = w.id)}
          >
            {w.label}
          </button>
        {/each}
      </nav>
    </div>
    <div class="title" data-live={st?.recording ?? false} data-rec={st?.session ?? false}>
      {#if st?.recording}<span class="dot" aria-hidden="true"></span>{/if}
      {title}
    </div>
    <div class="right">
      <ThemeMenu />
    </div>
  </header>

  <main>
    {#if page === 'home'}
      <Home />
    {:else if page === 'library'}
      <Library />
    {:else if page === 'settings'}
      <Settings />
    {/if}
  </main>

  {#if app.settings && !app.settings.app.first_run_done}
    <FirstRun s={app.settings} />
  {/if}

  <footer class="statusbar">
    <span>{app.error ?? hint}</span>
    <span class="grow"></span>
    {#if st && st.settings_issues.length > 0}
      <span class="warn">Settings repaired ({st.settings_issues.length})</span>
    {/if}
  </footer>
</div>

<style>
  .shell {
    display: grid;
    grid-template-rows: var(--header-h) 1fr var(--statusbar-h);
    height: 100%;
    gap: 2px;
  }
  .appbar {
    background: var(--chrome);
    display: grid;
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    padding: 0 var(--space-3);
  }
  .right {
    display: flex;
    justify-content: flex-end;
  }
  .left {
    display: flex;
    align-items: center;
    gap: var(--space-4);
  }
  .logo {
    width: 20px;
    height: 20px;
    border-radius: 5px;
    background: var(--accent);
    position: relative;
  }
  .logo::after {
    content: '';
    position: absolute;
    inset: 6px 5px 6px 7px;
    background: var(--on-accent);
    clip-path: polygon(0 0, 100% 50%, 0 100%);
  }
  nav {
    display: flex;
    gap: var(--space-2);
  }
  .ws {
    font-size: var(--text-lg);
    color: var(--text);
    position: relative;
    border-radius: 0;
  }
  .ws:hover:not(:disabled) {
    background: transparent;
    color: var(--text-strong);
  }
  .ws.active {
    color: var(--text-strong);
  }
  .ws.active::after {
    content: '';
    position: absolute;
    left: var(--space-2);
    right: var(--space-2);
    bottom: 0;
    height: 1.5px;
    background: var(--text-strong);
  }
  .title {
    font-size: var(--text-lg);
    color: var(--text-strong);
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--good);
  }
  [data-rec='true'] .dot {
    background: var(--rec);
  }
  main {
    min-height: 0;
  }
  .statusbar {
    background: var(--statusbar);
    color: var(--text-muted);
    font-size: var(--text-sm);
    display: flex;
    align-items: center;
    padding: 0 var(--space-3);
    gap: var(--space-3);
  }
  .grow {
    flex: 1;
  }
  .warn {
    color: var(--warn);
  }
</style>
