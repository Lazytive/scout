<!-- The overlay window: a "Clip saved" card, the recording dot, or the quick menu. The app
     sizes, places, shows and hides the window (never taking focus, click-through except
     for the menu); this page only draws what it is told on `overlay://show`. -->
<script lang="ts">
  import { onMount } from 'svelte';

  import { call, onEvent, pictureUrl } from './lib/api';
  import { applyLook, resolveLook, watchEnv, type Appearance, type CustomTheme } from './lib/appearance';
  import { formatDuration } from './lib/format';

  type Show =
    | {
        kind: 'clip';
        id: number;
        game: string | null;
        duration_ms: number;
        clip: string | null;
        thumb: string | null;
        seconds: number;
      }
    | { kind: 'thumb'; clip: string; data_url: string }
    | { kind: 'dot' }
    | { kind: 'menu'; game: string | null; recording: boolean; clips: { name: string; duration_ms: number }[] }
    | { kind: 'hide' };

  let view = $state<Show>({ kind: 'hide' });
  let thumb = $state<string | null>(null);

  const inTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
  // Linux (WebKitGTK): a web view with a see-through background draws only what is being
  // animated (the timer), so there the window is opaque and the pop-up fills it.
  if (typeof navigator !== 'undefined' && /\bLinux\b/.test(navigator.userAgent)) {
    document.documentElement.dataset.opaque = 'true';
  }
  // The overlay follows the theme (M10): colours, accent, corners, font and motion. Its
  // window is sized by the app, so text size, density and the picture stay out.
  let appearance: Partial<Appearance> | undefined;
  let custom: CustomTheme[] = [];
  const watcher = watchEnv(() => showTheme());
  function showTheme() {
    const look = resolveLook(appearance, custom, watcher.env(), pictureUrl);
    applyLook({ ...look, background: null, textScale: 1, density: 'comfortable' }, undefined, false);
  }
  async function loadTheme() {
    const [s, t] = await Promise.all([
      call<{ settings: { appearance?: Partial<Appearance> } }>('settings.get').catch(() => null),
      call<{ themes: CustomTheme[] }>('theme.list').catch(() => null),
    ]);
    if (s) appearance = s.settings.appearance;
    if (t) custom = t.themes;
    showTheme();
  }

  function show(s: Show) {
    if (s.kind === 'thumb') {
      if (view.kind === 'clip' && view.clip === s.clip) thumb = s.data_url;
      return;
    }
    if (s.kind === 'clip') thumb = s.thumb;
    view = s;
  }

  async function close() {
    if (!inTauri) return;
    const { invoke } = await import('@tauri-apps/api/core');
    await invoke('overlay_close').catch(() => undefined);
  }

  async function act(cmd: string) {
    await call(cmd).catch(() => undefined);
    await close();
  }

  onMount(() => {
    const stops: (() => void)[] = [];
    void loadTheme();
    void onEvent((e) => {
      if (e.event === 'settings_changed') {
        appearance = (e.settings as { appearance?: Partial<Appearance> }).appearance;
        showTheme();
      } else if (e.event === 'themes_changed') {
        void loadTheme();
      }
    }).then((u) => stops.push(u));
    if (inTauri) {
      void import('@tauri-apps/api/webviewWindow').then(({ getCurrentWebviewWindow }) =>
        getCurrentWebviewWindow()
          .listen<Show>('overlay://show', (e) => show(e.payload))
          .then((u) => stops.push(u)),
      );
    } else {
      // Browser preview: a sample card.
      show({ kind: 'clip', id: 1, game: 'ELDEN RING', duration_ms: 30_000, clip: null, thumb: null, seconds: 30 });
    }
    return () => {
      watcher.stop();
      stops.forEach((s) => s());
    };
  });
</script>

<svelte:window onkeydown={(e) => view.kind === 'menu' && e.key === 'Escape' && close()} />

{#if view.kind === 'clip'}
  {#key view.id}
    <div class="card" role="status" aria-live="polite">
      <div class="thumb" class:loaded={!!thumb}>
        {#if thumb}<img src={thumb} alt="" />{/if}
      </div>
      <div class="text">
        <div class="title"><span class="check" aria-hidden="true">✓</span> Clip saved</div>
        <div class="sub">
          {view.game ?? 'Desktop'} · {formatDuration(view.duration_ms)}
        </div>
      </div>
      <div class="timer" style:animation-duration="{view.seconds}s"></div>
    </div>
  {/key}
{:else if view.kind === 'dot'}
  <div class="dot" role="status" aria-label="Recording"></div>
{:else if view.kind === 'menu'}
  <div class="menu" role="dialog" aria-label="Scout quick menu">
    <header>
      <span class="logo" aria-hidden="true"></span>
      <span>{view.game ?? 'Scout'}</span>
      {#if view.recording}<span class="rec">● REC</span>{/if}
    </header>
    <div class="actions">
      <button class="primary" onclick={() => act('clip.save')}>Save clip</button>
      <button onclick={() => act('session.toggle')}>{view.recording ? 'Stop recording' : 'Record'}</button>
      <button onclick={() => act('bookmark.add')}>Bookmark</button>
    </div>
    <h2>Recent clips</h2>
    {#if view.clips.length === 0}
      <p class="muted">None yet.</p>
    {:else}
      <ul>
        {#each view.clips as c, i (i)}
          <li><span>{c.name}</span><span class="mono muted">{formatDuration(c.duration_ms)}</span></li>
        {/each}
      </ul>
    {/if}
    <p class="hint muted">Esc closes</p>
  </div>
{/if}

<style>
  :global(html),
  :global(body) {
    background: transparent !important;
    overflow: hidden;
  }
  /* Linux: an opaque window (see the script), so the card and the menu fill it. */
  :global(:root[data-opaque='true']),
  :global(:root[data-opaque='true'] body) {
    background: var(--panel) !important;
  }
  :global(:root[data-opaque='true']) .card,
  :global(:root[data-opaque='true']) .menu {
    inset: 0;
    border-radius: 0;
    box-shadow: none;
  }
  .card {
    position: absolute;
    inset: 6px;
    display: grid;
    grid-template-columns: 128px 1fr;
    gap: 12px;
    align-items: center;
    padding: 8px;
    background: color-mix(in srgb, var(--panel) 94%, transparent);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: 0 6px 18px rgb(0 0 0 / 40%);
    overflow: hidden;
    animation: enter 160ms ease-out;
  }
  .thumb {
    width: 128px;
    height: 72px;
    border-radius: 6px;
    background: linear-gradient(135deg, var(--panel-raised), var(--viewer));
    overflow: hidden;
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    animation: fade 200ms ease-out;
  }
  .text {
    min-width: 0;
  }
  .title {
    color: var(--text-strong);
    font-size: var(--text-lg);
    font-weight: 600;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .check {
    color: var(--good);
  }
  .sub {
    color: var(--text-muted);
    margin-top: 2px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .timer {
    position: absolute;
    left: 0;
    bottom: 0;
    height: 3px;
    width: 100%;
    background: var(--accent);
    transform-origin: left;
    animation: shrink linear forwards;
  }
  .dot {
    position: absolute;
    inset: 7px;
    border-radius: 50%;
    background: var(--rec);
    box-shadow: 0 0 0 2px rgb(0 0 0 / 45%);
  }
  .menu {
    position: absolute;
    inset: 6px;
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: 10px;
    box-shadow: 0 10px 28px rgb(0 0 0 / 50%);
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  header {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-strong);
    font-size: var(--text-lg);
    font-weight: 600;
  }
  .logo {
    width: 18px;
    height: 18px;
    border-radius: 5px;
    background: var(--accent);
  }
  .rec {
    margin-left: auto;
    color: var(--rec);
    font-size: var(--text-sm);
  }
  .actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }
  h2 {
    font-size: var(--text-sm);
    color: var(--text-muted);
    font-weight: 400;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  li {
    display: flex;
    justify-content: space-between;
    color: var(--text);
  }
  .hint {
    margin-top: auto;
    font-size: var(--text-xs);
  }
  @keyframes enter {
    from {
      opacity: 0;
      transform: translateY(-6px);
    }
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  @keyframes shrink {
    to {
      transform: scaleX(0);
    }
  }
  :global(:root[data-motion='reduce']) .card,
  :global(:root[data-motion='reduce']) .thumb img,
  :global(:root[data-motion='reduce']) .timer {
    animation: none !important;
  }
</style>
