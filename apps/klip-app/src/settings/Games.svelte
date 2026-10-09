<script lang="ts">
  import { onMount } from 'svelte';

  import { call } from '../lib/api';
  import { formatDuration } from '../lib/format';
  import ListEditor from '../lib/forms/ListEditor.svelte';
  import Row from '../lib/forms/Row.svelte';
  import Segmented from '../lib/forms/Segmented.svelte';
  import Select from '../lib/forms/Select.svelte';
  import Toggle from '../lib/forms/Toggle.svelte';
  import { bufferChoices, type Settings } from '../lib/settings';
  import { fieldError, save } from '../lib/store.svelte';

  let { s }: { s: Settings } = $props();
  const g = $derived(s.games);

  let programs = $state<{ exe: string; name: string; detected: boolean }[]>([]);
  let looking = $state(false);

  async function look() {
    looking = true;
    try {
      programs = (await call<{ programs: typeof programs }>('games.candidates')).programs;
    } finally {
      looking = false;
    }
  }
  onMount(() => void look());

  const key = (exe: string) => exe.trim().toLowerCase();
  const listed = (exe: string) => g.user_games.some((u) => key(u.exe) === key(exe));

  function addGame(exe: string, name: string | null) {
    if (!exe.trim() || listed(exe)) return;
    void save(['games', 'user_games'], [...g.user_games, { exe: exe.trim(), name }]);
  }

  // Per-game settings: the games on the list, games with a profile and detected ones.
  const profileGames = $derived(
    [
      ...new Map(
        [
          ...g.user_games.map((u) => [key(u.exe), u.name ?? u.exe] as const),
          ...programs.filter((p) => p.detected).map((p) => [key(p.exe), p.name] as const),
          ...Object.keys(g.profiles).map((k) => [k, k] as const),
        ].reverse(),
      ),
    ]
      .reverse()
      .map(([value, label]) => ({ value, label })),
  );
  let picked = $state('');
  const current = $derived(profileGames.some((p) => p.value === picked) ? picked : (profileGames[0]?.value ?? ''));
  const profile = $derived(current ? g.profiles[current] : undefined);

  const setProfile = (field: string, value: unknown) => save(['games', 'profiles', current, field], value);
</script>

<Row
  label="Replay buffer runs"
  hint={g.buffer === 'games'
    ? 'Only while a game runs: next to no CPU or memory otherwise.'
    : g.buffer === 'always'
      ? 'All the time; your main screen is recorded when no game runs.'
      : 'Never: hotkeys and the Save button do nothing.'}
>
  <Segmented
    label="When the replay buffer runs"
    value={g.buffer}
    options={[
      { value: 'games', label: 'While a game runs' },
      { value: 'always', label: 'Always' },
      { value: 'off', label: 'Off' },
    ]}
    onchange={(v) => save(['games', 'buffer'], v)}
  />
</Row>

<Row
  label="Store games"
  hint="Find games installed by Steam, Epic, GOG, Xbox, EA, Ubisoft, Riot, itch and Heroic by themselves."
  error={fieldError('games.detect_stores')}
>
  <Toggle
    label="Find games from store libraries"
    checked={g.detect_stores ?? true}
    onchange={(v) => save(['games', 'detect_stores'], v)}
  />
</Row>

<Row
  label="My games"
  hint={(g.detect_stores ?? true)
    ? 'Store games are found by themselves. Add anything else here.'
    : 'Only these programs are recorded.'}
  error={fieldError('games.user_games')}
>
  {#if g.user_games.length > 0}
    <ul class="games">
      {#each g.user_games as u (u.exe)}
        <li>
          <span class="name">{u.name ?? u.exe}</span>
          <span class="exe mono">{u.exe}</span>
          <button
            class="flat remove"
            onclick={() => save(['games', 'user_games'], g.user_games.filter((x) => x.exe !== u.exe))}
            >Remove</button
          >
        </li>
      {/each}
    </ul>
  {/if}
  <div class="running">
    <span class="muted">Running now:</span>
    {#each programs.filter((p) => !p.detected && !listed(p.exe)) as p (p.exe)}
      <button class="chip" onclick={() => addGame(p.exe, p.name)} title={p.exe}>+ {p.name}</button>
    {:else}
      <span class="muted">{looking ? 'Looking…' : 'nothing else with a window'}</span>
    {/each}
    <button class="flat refresh" onclick={look}>Refresh</button>
  </div>
  <ListEditor
    label="Add by program name"
    items={[]}
    placeholder="Game.exe"
    onchange={(v) => v[0] && addGame(v[0], null)}
  />
</Row>

<Row label="Never record" hint="Programs that look like games but aren't." error={fieldError('games.never')}>
  <ListEditor label="Never record" items={g.never} placeholder="Program.exe" onchange={(v) => save(['games', 'never'], v)} />
</Row>

<Row label="Per-game settings" hint="Override the buffer, quality or audio for one game." error={fieldError('games.profiles')}>
  {#if profileGames.length === 0}
    <p class="muted">Add a game first, or start one.</p>
  {:else}
    <Select label="Game" value={current} options={profileGames} onchange={(v) => (picked = v)} />
    <div class="profile">
      <span class="pl">Replay buffer</span>
      <Select
        label="Replay buffer for this game"
        value={profile?.buffer_secs ?? 0}
        options={[
          { value: 0, label: `Default (${formatDuration(s.recording.buffer_secs * 1000)})` },
          ...bufferChoices.map((v) => ({ value: v, label: formatDuration(v * 1000) })),
        ]}
        onchange={(v) => setProfile('buffer_secs', v || null)}
      />
      <span class="pl">Quality</span>
      <Select
        label="Quality for this game"
        value={profile?.quality?.preset ?? ''}
        options={[
          { value: '', label: 'Default' },
          { value: 'low', label: 'Low' },
          { value: 'balanced', label: 'Balanced' },
          { value: 'high', label: 'High' },
        ]}
        onchange={(v) => setProfile('quality', v ? { preset: v } : null)}
      />
      <span class="pl">Game sound</span>
      <Select
        label="Game sound for this game"
        value={profile?.desktop_audio ?? ''}
        options={[
          { value: '', label: 'Default' },
          { value: 'game', label: 'Game only' },
          { value: 'system', label: 'Everything' },
          { value: 'off', label: 'Off' },
        ]}
        onchange={(v) => setProfile('desktop_audio', v || null)}
      />
      <span class="pl">Microphone</span>
      <Select
        label="Microphone for this game"
        value={profile?.mic === null || profile?.mic === undefined ? '' : profile.mic ? 'on' : 'off'}
        options={[
          { value: '', label: 'Default' },
          { value: 'on', label: 'On' },
          { value: 'off', label: 'Off' },
        ]}
        onchange={(v) => setProfile('mic', v === '' ? null : v === 'on')}
      />
    </div>
    {#if profile}
      <button class="flat remove" onclick={() => save(['games', 'profiles', current], null)}>
        Use the defaults for this game
      </button>
    {/if}
  {/if}
</Row>

<style>
  ul.games {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .games li {
    display: grid;
    grid-template-columns: minmax(120px, 220px) minmax(0, 1fr) auto;
    gap: var(--space-3);
    align-items: center;
    padding: 3px 0;
  }
  .name {
    color: var(--text-strong);
  }
  .exe {
    color: var(--text-muted);
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .remove {
    color: var(--text-muted);
    align-self: flex-start;
  }
  .remove:hover {
    color: var(--bad);
  }
  .running {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
  }
  .chip {
    height: 24px;
    padding: 0 10px;
    font-size: var(--text-sm);
  }
  .refresh {
    color: var(--accent-text);
  }
  .profile {
    display: grid;
    grid-template-columns: 120px max-content;
    gap: 6px var(--space-3);
    align-items: center;
    margin-top: var(--space-2);
  }
  .pl {
    color: var(--text-muted);
  }
</style>
