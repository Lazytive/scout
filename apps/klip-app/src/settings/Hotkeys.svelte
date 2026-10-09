<script lang="ts">
  import { formatDuration } from '../lib/format';
  import KeyCapture from '../lib/forms/KeyCapture.svelte';
  import Select from '../lib/forms/Select.svelte';
  import { bindingError, type HotkeyAction, type HotkeyBinding, type Settings } from '../lib/settings';
  import { app, fieldError, save } from '../lib/store.svelte';

  let { s }: { s: Settings } = $props();

  const list = $derived(s.hotkeys);
  const bound = $derived(app.status?.hotkeys ?? []);

  const actions: { value: HotkeyAction; label: string }[] = [
    { value: 'clip.save', label: 'Save a clip' },
    { value: 'session.toggle', label: 'Start / stop recording' },
    { value: 'bookmark.add', label: 'Bookmark' },
    { value: 'overlay.menu', label: 'Quick menu' },
  ];
  const lengths = [10, 15, 30, 60, 120, 300].map((v) => ({ value: v, label: `last ${formatDuration(v * 1000)}` }));

  function update(i: number, change: Partial<HotkeyBinding>) {
    const next = list.map((b, j) => (j === i ? { ...b, ...change } : b));
    void save(['hotkeys'], next);
  }

  function remove(i: number) {
    void save(['hotkeys'], list.filter((_, j) => j !== i));
  }

  function add() {
    void save(['hotkeys'], [...list, { action: 'clip.save', keys: '', secs: 30 }]);
  }
</script>

<p class="intro muted">
  Hotkeys work while a game has focus. A combination you set here is taken from other apps while
  Klip runs, so pick one your games don't use.
</p>

<table>
  <thead>
    <tr><th>Action</th><th>Length</th><th>Keys</th><th></th></tr>
  </thead>
  <tbody>
    {#each list as b, i (i)}
      {@const err = fieldError(`hotkeys[${i}]`) ?? bindingError(b.keys, bound)}
      <tr>
        <td>
          <Select
            label="Action"
            value={b.action}
            options={actions}
            onchange={(v) => update(i, { action: v, secs: v === 'clip.save' ? b.secs || 30 : 0 })}
          />
        </td>
        <td>
          {#if b.action === 'clip.save'}
            <Select
              label="Clip length"
              value={b.secs}
              options={lengths.some((l) => l.value === b.secs) ? lengths : [...lengths, { value: b.secs, label: `last ${formatDuration(b.secs * 1000)}` }]}
              onchange={(v) => update(i, { secs: v })}
            />
          {/if}
        </td>
        <td>
          <KeyCapture label="Keys for {actions.find((x) => x.value === b.action)?.label}" keys={b.keys} onchange={(k) => update(i, { keys: k })} />
          {#if err}<p class="error">{err}</p>{/if}
        </td>
        <td><button class="flat remove" aria-label="Remove this hotkey" onclick={() => remove(i)}>Remove</button></td>
      </tr>
    {/each}
  </tbody>
</table>
<button onclick={add}>Add a hotkey</button>

<style>
  .intro {
    margin-bottom: var(--space-3);
    max-width: 640px;
  }
  table {
    border-collapse: collapse;
    margin-bottom: var(--space-4);
  }
  th {
    text-align: left;
    font-weight: 400;
    color: var(--text-muted);
    font-size: var(--text-sm);
    padding: 0 var(--space-4) 6px 0;
  }
  td {
    padding: 6px var(--space-4) 6px 0;
    vertical-align: top;
    border-top: 1px solid var(--border);
  }
  .error {
    color: var(--bad);
    font-size: var(--text-sm);
    margin-top: 4px;
  }
  .remove {
    color: var(--text-muted);
    padding-top: 6px;
  }
  .remove:hover {
    color: var(--bad);
  }
</style>
