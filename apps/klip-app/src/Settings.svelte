<!-- The Settings workspace: categories on the left, the chosen one's settings on the right.
     Every change is saved at once (a strict `settings.set`); a rejected value shows its
     reason next to the field. -->
<script lang="ts">
  import { onMount, type Component } from 'svelte';

  import { debugInfo } from './lib/api';

  import Panel from './lib/Panel.svelte';
  import type { Settings as SettingsData } from './lib/settings';
  import { app } from './lib/store.svelte';
  import Appearance from './settings/Appearance.svelte';
  import Audio from './settings/Audio.svelte';
  import Debug from './settings/Debug.svelte';
  import Games from './settings/Games.svelte';
  import General from './settings/General.svelte';
  import Hotkeys from './settings/Hotkeys.svelte';
  import Overlay from './settings/Overlay.svelte';
  import Recording from './settings/Recording.svelte';
  import Storage from './settings/Storage.svelte';

  type PageDef = { id: string; label: string; page: Component<{ s: SettingsData }> };
  const basePages: PageDef[] = [
    { id: 'general', label: 'General', page: General },
    { id: 'appearance', label: 'Appearance', page: Appearance },
    { id: 'recording', label: 'Recording', page: Recording },
    { id: 'audio', label: 'Audio', page: Audio },
    { id: 'hotkeys', label: 'Hotkeys', page: Hotkeys },
    { id: 'games', label: 'Games', page: Games },
    { id: 'storage', label: 'Storage', page: Storage },
    { id: 'overlay', label: 'Overlay', page: Overlay },
  ];
  // Debug (the self-test) only in tester builds (`full-logs`) and the browser mock.
  let showDebug = $state(false);
  onMount(() => {
    void debugInfo().then((i) => (showDebug = i.full_logs));
  });
  const pages = $derived<PageDef[]>(
    showDebug ? [...basePages, { id: 'debug', label: 'Debug', page: Debug }] : basePages,
  );

  let { initial = 'general' }: { initial?: string } = $props();
  const current = $derived(pages.find((p) => p.id === (app.settingsPage || initial)) ?? pages[0]!);
</script>

<div class="workspace">
  <nav class="cats" aria-label="Settings categories">
    <Panel tabs={['Settings']}>
      <ul>
        {#each pages as p (p.id)}
          <li>
            <button
              class="flat cat"
              class:active={current.id === p.id}
              aria-current={current.id === p.id ? 'page' : undefined}
              onclick={() => (app.settingsPage = p.id)}>{p.label}</button
            >
          </li>
        {/each}
      </ul>
    </Panel>
  </nav>
  <section class="page">
    <Panel tabs={[current.label]}>
      {#if app.settings}
        {@const Page = current.page}
        <div class="content">
          <Page s={app.settings} />
        </div>
      {:else}
        <p class="muted">Loading the settings…</p>
      {/if}
    </Panel>
  </section>
</div>

<style>
  .workspace {
    display: grid;
    grid-template-columns: 200px minmax(0, 1fr);
    gap: 2px;
    height: 100%;
  }
  .cats,
  .page {
    display: flex;
    min-height: 0;
    min-width: 0;
  }
  .cats > :global(*),
  .page > :global(*) {
    flex: 1;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .cat {
    width: 100%;
    justify-content: flex-start;
    color: var(--text);
    padding: 7px var(--space-3);
    border-radius: var(--radius-sm);
  }
  .cat:hover {
    background: var(--hover);
    color: var(--text-strong);
  }
  .cat.active {
    background: var(--accent);
    color: var(--on-accent);
  }
  .content {
    max-width: 860px;
    padding: 0 var(--space-2) var(--space-5);
  }
</style>
