<!-- The Library workspace, Scout's start page (M11.2: the old Home folded in; its
     recorder bar lives in the app's title bar since M11.3): filters on the left, the clip grid (search, sort, card size, day headings,
     hover-scrub, multi-select, keyboard, context menu) and the details of the selection
     on the right, docked like the Photocraft / FilmCraft panels. -->
<script lang="ts">
  import { onMount } from 'svelte';

  import AsciiWater from './library/AsciiWater.svelte';
  import ClipCard from './library/ClipCard.svelte';
  import Details from './library/Details.svelte';
  import Filters from './library/Filters.svelte';
  import { openEditor } from './lib/editor.svelte';
  import { formatBytes, formatDuration } from './lib/format';
  import Select from './lib/forms/Select.svelte';
  import { clickSelect, filtered, groupClips, moveKey, type LibClip, type Sort } from './lib/library';
  import {
    askDelete,
    clipByKey,
    confirmDelete,
    copyClips,
    discord,
    discordClip,
    dragClipsOut,
    isFresh,
    lib,
    openClip,
    refreshLibrary,
    selectedClips,
    setQuery,
    setSize,
    setStar,
    type CardSize,
  } from './lib/library.svelte';
  import Panel from './lib/Panel.svelte';
  import { defaultQuery } from './lib/library';
  import { app } from './lib/store.svelte';

  const trashName = typeof navigator !== 'undefined' && /Windows/i.test(navigator.userAgent) ? 'Recycle Bin' : 'Trash';

  const sorts: { value: Sort; label: string }[] = [
    { value: 'newest', label: 'Newest first' },
    { value: 'oldest', label: 'Oldest first' },
    { value: 'longest', label: 'Longest first' },
    { value: 'shortest', label: 'Shortest first' },
    { value: 'largest', label: 'Largest first' },
    { value: 'name', label: 'Name' },
  ];
  const sizes: { id: CardSize; label: string; min: number }[] = [
    { id: 's', label: 'S', min: 150 },
    { id: 'm', label: 'M', min: 200 },
    { id: 'l', label: 'L', min: 280 },
  ];

  const listing = $derived(lib.listing);
  const clips = $derived(listing?.clips ?? []);
  const order = $derived(clips.map((c) => c.key));
  const groups = $derived(groupClips(clips, lib.query.sort, new Date()));
  const tiles = $derived(listing?.strip.tiles ?? 16);
  const minWidth = $derived(sizes.find((s) => s.id === lib.size)?.min ?? 230);

  // The first working clip hotkey, for the "save your first clip" hint.
  const firstKey = $derived(
    (app.status?.hotkeys ?? []).find((h) => h.action === 'clip.save' && h.secs && h.keys && !h.error) ?? null,
  );

  let grid: HTMLElement | undefined = $state();
  let search = $state(lib.query.search);
  let menu = $state<{ x: number; y: number; key: string } | null>(null);
  let renameFocus = $state(0);
  let lastFocusRefresh = 0;

  function pick(c: LibClip, e: MouseEvent) {
    const r = clickSelect(lib.selected, lib.anchor, c.key, order, {
      ctrl: e.ctrlKey || e.metaKey,
      shift: e.shiftKey,
    });
    lib.selected = r.selected;
    lib.anchor = r.anchor;
    lib.cursor = c.key;
    grid?.focus({ preventScroll: true });
  }

  function openMenu(c: LibClip, e: MouseEvent) {
    e.preventDefault();
    if (!lib.selected.includes(c.key)) {
      lib.selected = [c.key];
      lib.anchor = c.key;
    }
    lib.cursor = c.key;
    menu = { x: e.clientX, y: e.clientY, key: c.key };
  }

  function columns(): number {
    if (!grid) return 1;
    const first = grid.querySelector<HTMLElement>('.cards');
    if (!first) return 1;
    return Math.max(1, getComputedStyle(first).gridTemplateColumns.split(' ').filter(Boolean).length);
  }

  function keydown(e: KeyboardEvent) {
    const dirs: Record<string, 'left' | 'right' | 'up' | 'down' | 'home' | 'end'> = {
      ArrowLeft: 'left',
      ArrowRight: 'right',
      ArrowUp: 'up',
      ArrowDown: 'down',
      Home: 'home',
      End: 'end',
    };
    const dir = dirs[e.key];
    if (dir) {
      e.preventDefault();
      const next = moveKey(lib.cursor ?? lib.anchor, order, dir === 'up' || dir === 'down' ? columns() : 1, dir);
      if (next) {
        if (e.shiftKey && lib.anchor !== null) {
          // The range keeps its start; the cursor moves.
          lib.selected = clickSelect(lib.selected, lib.anchor, next, order, { ctrl: false, shift: true }).selected;
        } else {
          lib.selected = [next];
          lib.anchor = next;
        }
        lib.cursor = next;
      }
      return;
    }
    const sel = selectedClips();
    switch (e.key) {
      case 'Enter':
        if (sel.length === 1 && sel[0]) openEditor(sel[0]);
        break;
      case 'Delete':
        askDelete(lib.selected, e.shiftKey);
        break;
      case 'F2':
        if (sel.length === 1) renameFocus += 1;
        break;
      case 'Escape':
        lib.selected = [];
        menu = null;
        break;
      case 'a':
        if (e.ctrlKey || e.metaKey) {
          e.preventDefault();
          lib.selected = [...order];
        }
        break;
      case 'c':
        if ((e.ctrlKey || e.metaKey) && sel.length > 0) {
          e.preventDefault();
          void copyClips(sel);
        }
        break;
      default:
        return;
    }
  }

  function clearFilters() {
    search = '';
    setQuery({ ...defaultQuery, sort: lib.query.sort });
  }

  onMount(() => {
    lib.active = true;
    void refreshLibrary();
    // Clips may have been added or removed outside Scout while the window was away.
    const onFocus = () => {
      if (Date.now() - lastFocusRefresh > 5000) {
        lastFocusRefresh = Date.now();
        void refreshLibrary();
      }
    };
    window.addEventListener('focus', onFocus);
    const close = () => (menu = null);
    window.addEventListener('blur', close);
    return () => {
      window.removeEventListener('focus', onFocus);
      window.removeEventListener('blur', close);
    };
  });
</script>

<svelte:window onclick={() => (menu = null)} />

<div class="workspace">
  <div class="left">
    <Panel tabs={['Browse']}>
      <Filters />
    </Panel>
  </div>

  <div class="center">
    <Panel tabs={['Clips']}>
      {#snippet actions()}
        <span class="count mono">
          {#if listing}
            {listing.matched === listing.total ? listing.total : `${listing.matched} of ${listing.total}`} clips · {formatBytes(
              listing.total_bytes,
            )}
          {/if}
        </span>
      {/snippet}
      <div class="body">
        <div class="toolbar">
          <input
            class="search"
            type="search"
            placeholder="Search names, games, tags"
            aria-label="Search clips"
            spellcheck="false"
            bind:value={search}
            oninput={() => setQuery({ search })}
            onkeydown={(e) => {
              if (e.key === 'Escape') {
                search = '';
                setQuery({ search: '' });
              }
            }}
          />
          <Select label="Sort" value={lib.query.sort} options={sorts} onchange={(v) => setQuery({ sort: v })} />
          <span class="grow"></span>
          <div class="sizes" role="radiogroup" aria-label="Card size">
            {#each sizes as s (s.id)}
              <button
                class="flat"
                role="radio"
                aria-checked={lib.size === s.id}
                class:on={lib.size === s.id}
                title="{s.label === 'S' ? 'Small' : s.label === 'M' ? 'Medium' : 'Large'} cards"
                onclick={() => setSize(s.id)}>{s.label}</button
              >
            {/each}
          </div>
        </div>
        {#if lib.error}<p class="error" role="alert">{lib.error}</p>{/if}

        <div class="grid-area">
          {#if lib.water}<AsciiWater />{/if}
          <div
            class="grid"
            bind:this={grid}
            role="listbox"
            aria-label="Clips"
            aria-multiselectable="true"
            tabindex="0"
            onkeydown={keydown}
            style:--card-min="{minWidth}px"
          >
            {#if !listing}
              <p class="empty-state muted">{lib.loading ? 'Reading the clip folder…' : ''}</p>
            {:else if listing.total === 0}
              <div class="empty-state">
                <p class="big">No clips yet</p>
                <p class="muted">
                  {#if firstKey}
                    Press <span class="combo"
                      >{#each (firstKey.keys ?? '').split('+') as k, i (i)}<kbd>{k}</kbd>{/each}</span
                    >
                    in a game to save the last {formatDuration((firstKey.secs ?? 30) * 1000)}, or use <b>Save</b> at the top.
                  {:else}
                    Use <b>Save</b> at the top while a game runs.
                  {/if}
                </p>
                <p class="muted">
                  Your clips show up here, newest first. Scout keeps them as plain MP4 files in {listing.folder}.
                </p>
              </div>
            {:else if clips.length === 0}
              <div class="empty-state">
                <p class="big">No clips match</p>
                {#if filtered(lib.query)}<button onclick={clearFilters}>Clear filters</button>{/if}
              </div>
            {:else}
              {#each groups as g (g.key)}
                <section aria-label={g.label ?? 'Clips'}>
                  {#if g.label}
                    <h2>{g.label}<span class="n mono">{g.clips.length}</span></h2>
                  {/if}
                  <div class="cards">
                    {#each g.clips as c, i (c.key)}
                      <ClipCard
                        clip={c}
                        {tiles}
                        order={i}
                        fresh={isFresh(c.path)}
                        selected={lib.selected.includes(c.key)}
                        focused={lib.cursor === c.key}
                        showDay={g.label === null}
                        onpick={(e) => pick(c, e)}
                        onopen={() => openEditor(c)}
                        onmenu={(e) => openMenu(c, e)}
                        onstar={() => setStar([c], !c.starred)}
                        ondragout={() => void dragClipsOut(c)}
                        progress={discord.job?.key === c.key ? discord.job.pct : null}
                      />
                    {/each}
                  </div>
                </section>
              {/each}
            {/if}
          </div>
        </div>
      </div>
    </Panel>
  </div>

  <div class="right">
    <Panel tabs={['Details']}>
      <Details {trashName} {renameFocus} />
    </Panel>
  </div>
</div>

{#if menu}
  {@const c = clipByKey(menu.key)}
  {@const many = lib.selected.length > 1}
  {#if c}
    <div
      class="menu"
      role="menu"
      tabindex="-1"
      style:left="{Math.min(menu.x, window.innerWidth - 220)}px"
      style:top="{Math.min(menu.y, window.innerHeight - 220)}px"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => e.key === 'Escape' && (menu = null)}
    >
      {#if !many}
        <button
          class="flat item"
          role="menuitem"
          onclick={() => {
            menu = null;
            openEditor(c);
          }}>Edit</button
        >
        <button
          class="flat item"
          role="menuitem"
          onclick={() => {
            menu = null;
            void openClip(c);
          }}>Open in player</button
        >
        <button
          class="flat item"
          role="menuitem"
          onclick={() => {
            menu = null;
            void openClip(c, true);
          }}>Show in folder</button
        >
        <button
          class="flat item"
          role="menuitem"
          onclick={() => {
            menu = null;
            renameFocus += 1;
          }}>Rename<kbd>F2</kbd></button
        >
      {/if}
      <button
        class="flat item"
        role="menuitem"
        onclick={() => {
          menu = null;
          void copyClips(selectedClips());
        }}>{many ? `Copy ${lib.selected.length} files` : 'Copy file'}<kbd>Ctrl+C</kbd></button
      >
      {#if !many}
        <button
          class="flat item"
          role="menuitem"
          disabled={discord.job !== null}
          onclick={() => {
            menu = null;
            void discordClip(c);
          }}>Copy for Discord</button
        >
      {/if}
      <button
        class="flat item"
        role="menuitem"
        onclick={() => {
          menu = null;
          const sel = selectedClips();
          void setStar(sel, !sel.every((x) => x.starred));
        }}>{selectedClips().every((x) => x.starred) ? 'Unstar' : 'Star'}</button
      >
      <hr />
      <button
        class="flat item bad"
        role="menuitem"
        onclick={() => {
          menu = null;
          askDelete(lib.selected, false);
        }}>Move to {trashName}<kbd>Del</kbd></button
      >
      <button
        class="flat item bad"
        role="menuitem"
        onclick={() => {
          menu = null;
          askDelete(lib.selected, true);
        }}>Delete permanently<kbd>Shift+Del</kbd></button
      >
    </div>
  {/if}
{/if}

{#if lib.confirm}
  {@const n = lib.confirm.keys.length}
  {@const what = n === 1 ? `“${clipByKey(lib.confirm.keys[0] ?? '')?.name ?? 'this clip'}”` : `${n} clips`}
  <div class="scrim" role="presentation" onclick={() => (lib.confirm = null)}>
    <div
      class="dialog"
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="del-title"
      tabindex="-1"
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => {
        if (e.key === 'Escape') lib.confirm = null;
        if (e.key === 'Enter') void confirmDelete();
      }}
    >
      {#if lib.confirm.permanent}
        <h2 id="del-title">Delete {what} permanently?</h2>
        <p class="muted">This can't be undone.</p>
      {:else}
        <h2 id="del-title">Move {what} to the {trashName}?</h2>
        <p class="muted">You can restore {n === 1 ? 'it' : 'them'} from the {trashName}.</p>
      {/if}
      <div class="buttons">
        <button onclick={() => (lib.confirm = null)}>Cancel</button>
        <!-- svelte-ignore a11y_autofocus -->
        <button class="primary" class:destroy={lib.confirm.permanent} autofocus onclick={() => void confirmDelete()}>
          {lib.confirm.permanent ? 'Delete permanently' : `Move to ${trashName}`}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  .workspace {
    display: grid;
    grid-template-columns: 220px minmax(0, 1fr) 320px;
    gap: 2px;
    height: 100%;
  }
  .combo {
    display: inline-flex;
    gap: 3px;
    margin: 0 2px;
  }
  .left,
  .center,
  .right {
    display: flex;
    min-height: 0;
    min-width: 0;
  }
  .left > :global(*),
  .center > :global(*),
  .right > :global(*) {
    flex: 1;
  }
  .count {
    align-self: center;
    font-size: var(--text-xs);
    color: var(--text-muted);
    padding-right: var(--space-1);
  }
  .body {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding-bottom: var(--space-2);
    flex: none;
  }
  .search {
    font: inherit;
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: 999px;
    height: var(--control-h);
    padding: 0 12px;
    width: min(320px, 40%);
    user-select: text;
  }
  .toolbar :global(select) {
    min-width: 150px;
  }
  .grow {
    flex: 1;
  }
  .sizes {
    display: inline-flex;
    border: 1.5px solid var(--border-strong);
    border-radius: 999px;
    padding: 2px;
    gap: 2px;
  }
  .sizes button {
    color: var(--text);
    border-radius: 999px;
    padding: 2px 10px;
    font-size: var(--text-sm);
  }
  .sizes button.on {
    background: var(--accent);
    color: var(--on-accent);
  }
  .error {
    color: var(--bad);
    font-size: var(--text-sm);
    padding-bottom: var(--space-2);
  }
  /* Holds the clip grid over the ASCII water (when on). */
  .grid-area {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .grid {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    outline: none;
    margin: 0 calc(-1 * var(--space-2));
    padding: 0 var(--space-2) var(--space-4);
  }
  .grid:focus-visible {
    box-shadow: inset 0 0 0 1px var(--focus);
  }
  h2 {
    font-size: var(--text-md);
    font-weight: 600;
    color: var(--text-strong);
    margin: var(--space-3) 0 var(--space-1) 6px;
    display: flex;
    align-items: baseline;
    gap: var(--space-2);
  }
  section:first-child h2 {
    margin-top: var(--space-1);
  }
  h2 .n {
    font-size: var(--text-xs);
    font-weight: 400;
    color: var(--text-muted);
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(var(--card-min), 1fr));
    gap: 4px;
  }
  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--space-3);
    padding: 80px var(--space-5);
    text-align: center;
  }
  .empty-state .muted {
    max-width: 440px;
  }
  .big {
    font-size: var(--text-lg);
    color: var(--text-strong);
  }
  .menu {
    animation: pop-in var(--motion) var(--ease-out);
    transform-origin: top left;
    position: fixed;
    z-index: 20;
    min-width: 210px;
    background: var(--panel-raised);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 4px;
    box-shadow: 0 8px 24px rgb(0 0 0 / 35%);
    display: flex;
    flex-direction: column;
  }
  .item {
    justify-content: space-between;
    color: var(--text-strong);
    padding: 6px 10px;
    width: 100%;
  }
  .item:hover {
    background: var(--hover);
  }
  .item.bad {
    color: var(--bad);
  }
  .item kbd {
    margin-left: var(--space-4);
  }
  hr {
    border: none;
    border-top: 1px solid var(--border);
    margin: 4px 2px;
  }
  .scrim {
    animation: fade-in var(--motion) ease-out;
    position: fixed;
    inset: 0;
    background: rgb(0 0 0 / 45%);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 30;
  }
  .dialog {
    animation: pop-in var(--motion-enter) var(--ease-out);
    background: var(--panel);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: var(--space-5);
    width: min(440px, 90vw);
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    box-shadow: 0 16px 48px rgb(0 0 0 / 45%);
    outline: none;
  }
  .dialog h2 {
    margin: 0;
    font-size: var(--text-lg);
    overflow-wrap: anywhere;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
    margin-top: var(--space-3);
  }
  .destroy {
    background: var(--bad);
    border-color: var(--bad);
  }
  .destroy:hover:not(:disabled) {
    background: var(--bad);
    border-color: var(--bad);
    filter: brightness(0.9);
  }
</style>
