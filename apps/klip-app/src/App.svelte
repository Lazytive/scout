<script lang="ts">
  import { onMount } from 'svelte';

  import Editor from './Editor.svelte';
  import FirstRun from './FirstRun.svelte';
  import Library from './Library.svelte';
  import RecorderBar from './library/RecorderBar.svelte';
  import { ed, openEditorByPath } from './lib/editor.svelte';
  import { formatDuration } from './lib/format';
  import ThemeMenu from './lib/ThemeMenu.svelte';
  import Settings from './Settings.svelte';
  import Toast from './lib/Toast.svelte';
  import { customChrome, dragsWindow, startDrag, toggleMaximize } from './lib/window-chrome';
  import WindowButtons from './lib/WindowButtons.svelte';
  import { app, connect, type Page } from './lib/store.svelte';

  // KLIP_START_PAGE / KLIP_START_CLIP (tests): the app injects the page (and clip) to open on.
  const start = (globalThis as { __KLIP_START_PAGE__?: string; __KLIP_START_CLIP__?: string }).__KLIP_START_PAGE__;
  const startClip = (globalThis as { __KLIP_START_CLIP__?: string }).__KLIP_START_CLIP__;
  if (start === 'settings') app.page = start;

  const workspaces = $derived<{ id: Page; label: string; soon?: boolean }[]>([
    { id: 'library', label: 'Library' },
    ...(ed.clip ? [{ id: 'editor' as const, label: 'Editor' }] : []),
    { id: 'settings', label: 'Settings' },
  ]);

  const st = $derived(app.status);

  // Scout's own title bar (Windows, M11.3): its empty parts move the window, a double
  // press maximizes, like the system's.
  function barDown(e: MouseEvent) {
    if (!customChrome || e.button !== 0 || !dragsWindow(e.target as Element | null)) return;
    e.preventDefault();
    if (e.detail === 2) void toggleMaximize();
    else void startDrag();
  }

  const hint = $derived.by(() => {
    const clip = st?.hotkeys.find((h) => h.action === 'clip.save' && !h.error);
    if (!st?.recording) return 'Scout starts the replay buffer by itself when a game opens.';
    if (clip?.keys && clip.secs) return `${clip.keys} saves the last ${formatDuration(clip.secs * 1000)}.`;
    return 'Save a clip with the button or a hotkey.';
  });

  onMount(() => {
    let stop: (() => void) | undefined;
    void connect().then((s) => {
      stop = s;
      if (start === 'editor' && startClip) void openEditorByPath(startClip);
    });
    return () => stop?.();
  });
</script>

<div class="shell">
  <!-- One bar (M11.3): workspaces, the recorder (M11.2) and, on Windows, the window's
       own buttons in place of the system title bar. -->
  <header class="appbar" class:chrome={customChrome} role="presentation" onmousedown={barDown}>
    <div class="left">
      <span class="logo" aria-hidden="true"></span>
      <nav aria-label="Workspaces">
        {#each workspaces as w (w.id)}
          <button
            class="flat ws"
            class:active={app.page === w.id}
            disabled={w.soon}
            title={w.soon ? 'Coming soon' : undefined}
            aria-current={app.page === w.id ? 'page' : undefined}
            onclick={() => (app.page = w.id)}
          >
            {w.label}
          </button>
        {/each}
      </nav>
    </div>
    <RecorderBar />
    <div class="right">
      <ThemeMenu />
    </div>
    {#if customChrome}<WindowButtons />{/if}
  </header>

  <main>
    {#if app.page === 'library'}
      <Library />
    {:else if app.page === 'editor'}
      <Editor />
    {:else if app.page === 'settings'}
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
  <Toast />
</div>

<style>
  .shell {
    display: grid;
    grid-template-rows: var(--header-h) 1fr var(--statusbar-h);
    height: 100%;
    gap: 2px;
  }
  .appbar {
    position: relative;
    z-index: 10;
    background: var(--chrome);
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-width: 0;
    padding: 0 var(--space-3);
    user-select: none;
  }
  .appbar.chrome {
    padding-right: 0;
  }
  .right {
    display: flex;
    flex: none;
  }
  .left {
    display: flex;
    align-items: center;
    flex: none;
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
