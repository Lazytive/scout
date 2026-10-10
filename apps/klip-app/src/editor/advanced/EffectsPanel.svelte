<!--
  The whole edit's look and sound: the colour grade (brightness, contrast, saturation
  over every picture, under the text), music ducking under voices, and markers (yours to
  rename and remove; the clips' bookmarks and events as they fall in the edit).
-->
<script lang="ts">
  import { readAdvanced, removeMarker, setColour, setDucking, updateMarker } from '../../lib/advanced';
  import type { Composition } from '../../lib/compose';
  import { ed, edit, liveEdit, seek } from '../../lib/editor.svelte';
  import Slider from '../../lib/forms/Slider.svelte';
  import Toggle from '../../lib/forms/Toggle.svelte';
  import { formatTimecode } from '../../lib/format';
  import type { Project } from '../../lib/timeline';

  let { project, comp }: { project: Project; comp: Composition } = $props();

  const adv = $derived(readAdvanced(project));
  const pct = (v: number) => `${Math.round(v * 100)} %`;
  const signed = (v: number) => `${v > 0 ? '+' : ''}${Math.round(v * 100)}`;
  const others = $derived(comp.markers.filter((m) => m.kind !== 'user'));
</script>

<section>
  <h3>Colour</h3>
  <Slider label="Brightness" value={adv.colour.brightness} min={-1} max={1} step={0.01} format={signed} onchange={(v) => liveEdit((p) => setColour(p, { brightness: v }), 'Brightness')} />
  <Slider label="Contrast" value={adv.colour.contrast} min={0} max={2} step={0.01} format={pct} onchange={(v) => liveEdit((p) => setColour(p, { contrast: v }), 'Contrast')} />
  <Slider label="Saturation" value={adv.colour.saturation} min={0} max={2} step={0.01} format={pct} onchange={(v) => liveEdit((p) => setColour(p, { saturation: v }), 'Saturation')} />
  <button class="flat" onclick={() => edit((p) => setColour(p, { brightness: 0, contrast: 1, saturation: 1 }), 'Reset colour')}>Reset</button>
</section>

<section>
  <h3>Music under voices</h3>
  <Toggle label="Duck the music while someone speaks" checked={adv.ducking.enabled} onchange={(v) => edit((p) => setDucking(p, { enabled: v }), v ? 'Ducking on' : 'Ducking off')} />
  <Slider label="Drop" value={adv.ducking.duck_db} min={-40} max={0} step={1} format={(v) => `${Math.round(v)} dB`} disabled={!adv.ducking.enabled} onchange={(v) => liveEdit((p) => setDucking(p, { duck_db: v }), 'Ducking')} />
  <Slider label="Attack" value={adv.ducking.attack_ms} min={0} max={1000} step={10} format={(v) => `${Math.round(v)} ms`} disabled={!adv.ducking.enabled} onchange={(v) => liveEdit((p) => setDucking(p, { attack_ms: v }), 'Ducking')} />
  <Slider label="Release" value={adv.ducking.release_ms} min={0} max={3000} step={50} format={(v) => `${Math.round(v)} ms`} disabled={!adv.ducking.enabled} onchange={(v) => liveEdit((p) => setDucking(p, { release_ms: v }), 'Ducking')} />
</section>

<section>
  <h3>Markers</h3>
  {#if adv.markers.length === 0 && others.length === 0}
    <p class="hint muted">Press M to mark the playhead.</p>
  {/if}
  <ul>
    {#each adv.markers as m, i (i)}
      <li>
        <button class="flat tiny mono" onclick={() => {
          ed.playing = false;
          seek(m.at_ms);
        }}>{formatTimecode(m.at_ms)}</button>
        <input
          class="text"
          value={m.title}
          maxlength="120"
          aria-label="Marker title"
          onchange={(e) => edit((p) => updateMarker(p, i, { title: (e.currentTarget as HTMLInputElement).value }), 'Rename marker')}
        />
        <button class="flat tiny" title="Remove" onclick={() => edit((p) => removeMarker(p, i), 'Remove marker')}>×</button>
      </li>
    {/each}
    {#each others as m, i (`o${i}`)}
      <li class="other">
        <button class="flat tiny mono" onclick={() => {
          ed.playing = false;
          seek(m.at_ms);
        }}>{formatTimecode(m.at_ms)}</button>
        <span class="title {m.kind}">{m.title}</span>
      </li>
    {/each}
  </ul>
</section>

<style>
  section {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding-bottom: var(--space-3);
    margin-bottom: var(--space-3);
    border-bottom: 1px solid var(--border);
  }
  h3 {
    margin: 0;
    font-size: var(--text-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-muted);
  }
  .hint {
    margin: 0;
    font-size: var(--text-sm);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  li {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .text {
    flex: 1;
    min-width: 0;
    font: inherit;
    color: var(--text-strong);
    background: var(--input);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    height: 24px;
    padding: 0 6px;
  }
  .tiny {
    min-width: 0;
    padding: 0 5px;
    height: 22px;
    font-size: var(--text-xs);
  }
  .mono {
    font-family: var(--font-mono);
  }
  .title {
    font-size: var(--text-sm);
  }
  .title.bookmark {
    color: var(--accent-text);
  }
  .title.event {
    color: var(--good);
  }
</style>
