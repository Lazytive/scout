/**
 * The preview's sound (M9.5): every playing piece of sound is a media element routed
 * through Web Audio, weighed like the export's mixer (`klip_export::mixer`): Normal
 * mode's Game/Mic sliders for the main clip's tracks (the Mix track only when the clip
 * has no separate ones), the layer's volume keyframes, a muted track silent. Music goes
 * through its own bus, which ducks while the voice (microphone tracks, sound layers)
 * is above −45 dBFS, with the project's attack/release as one-pole time constants.
 */

import type { Ducking } from '../../lib/advanced';
import type { Volumes } from '../../lib/timeline';

/** The voice counts as speaking above this level (`mixer::VOICE_GATE_DB`). */
export const VOICE_GATE_DB = -45;

/** `mixer::volume_for`: which slider a recorded track follows (`null`: left out). */
export function trackWeight(name: string, hasSeparate: boolean, v: Volumes): number | null {
  if (name === 'Game' || name === 'System') return v.game;
  if (name === 'Microphone' || name === 'Voice') return v.mic;
  return hasSeparate ? null : v.game;
}

export function isVoice(name: string): boolean {
  return name === 'Microphone' || name === 'Voice';
}

export function hasSeparateTracks(names: string[]): boolean {
  return names.some((n) => n === 'Game' || n === 'System' || n === 'Microphone' || n === 'Voice');
}

export function dbToGain(db: number): number {
  return Math.pow(10, db / 20);
}

/** RMS of a block in dBFS (−Infinity for silence). */
export function rmsDb(block: Float32Array): number {
  if (block.length === 0) return -Infinity;
  let sum = 0;
  for (const v of block) sum += v * v;
  return 10 * Math.log10(sum / block.length);
}

export type Bus = 'main' | 'music';

/** One element's way into the mix. */
export interface Strand {
  el: HTMLMediaElement;
  source: MediaElementAudioSourceNode;
  gain: GainNode;
  bus: Bus;
  voice: boolean;
}

/** The Web Audio graph shared by everything the preview plays. */
export class SoundGraph {
  private ctx: AudioContext | null = null;
  private mainBus: GainNode | null = null;
  private musicBus: GainNode | null = null;
  private duck: GainNode | null = null;
  private voiceBus: GainNode | null = null;
  private analyser: AnalyserNode | null = null;
  private block: Float32Array<ArrayBuffer> | null = null;
  private strands = new Map<HTMLMediaElement, Strand>();
  private ducking: Ducking = { enabled: false, duck_db: -12, attack_ms: 100, release_ms: 600 };
  private duckTarget = 1;
  private master = 1;

  /** The context, made on first use (after a user gesture: Play). */
  private context(): AudioContext | null {
    if (this.ctx) return this.ctx;
    try {
      const ctx = new AudioContext({ latencyHint: 'interactive' });
      const mainBus = ctx.createGain();
      const musicBus = ctx.createGain();
      const duck = ctx.createGain();
      const voiceBus = ctx.createGain();
      const analyser = ctx.createAnalyser();
      analyser.fftSize = 512;
      const sink = ctx.createGain();
      sink.gain.value = 0;
      mainBus.connect(ctx.destination);
      musicBus.connect(duck).connect(ctx.destination);
      voiceBus.connect(analyser).connect(sink).connect(ctx.destination);
      this.ctx = ctx;
      this.mainBus = mainBus;
      this.musicBus = musicBus;
      this.duck = duck;
      this.voiceBus = voiceBus;
      this.analyser = analyser;
      this.block = new Float32Array(analyser.fftSize);
      this.setMaster(this.master);
    } catch {
      this.ctx = null;
    }
    return this.ctx;
  }

  /** Resume after a gesture (browsers start contexts suspended). */
  resume(): void {
    const ctx = this.context();
    if (ctx && ctx.state === 'suspended') void ctx.resume().catch(() => {});
  }

  setMaster(v: number): void {
    this.master = v;
    if (this.mainBus) this.mainBus.gain.value = v;
    if (this.musicBus) this.musicBus.gain.value = v;
  }

  setDucking(d: Ducking): void {
    this.ducking = d;
    if (!d.enabled) this.duckTarget = 1;
  }

  /** Route `el` into the mix (once; later calls update bus and voice). */
  attach(el: HTMLMediaElement, bus: Bus, voice: boolean): Strand | null {
    const ctx = this.context();
    if (!ctx || !this.mainBus || !this.musicBus || !this.voiceBus) return null;
    const route = (st: Strand) => {
      st.gain.connect(st.bus === 'music' ? this.musicBus! : this.mainBus!);
      if (st.voice) st.gain.connect(this.voiceBus!);
    };
    let s = this.strands.get(el);
    if (!s) {
      let source: MediaElementAudioSourceNode;
      try {
        source = ctx.createMediaElementSource(el);
      } catch {
        return null;
      }
      const gain = ctx.createGain();
      gain.gain.value = 0;
      source.connect(gain);
      s = { el, source, gain, bus, voice };
      this.strands.set(el, s);
      route(s);
    } else if (s.bus !== bus || s.voice !== voice) {
      s.gain.disconnect();
      s.bus = bus;
      s.voice = voice;
      route(s);
    }
    return s;
  }

  /** Set a strand's gain now (a short ramp avoids clicks). */
  setGain(el: HTMLMediaElement, v: number): void {
    const s = this.strands.get(el);
    if (!s || !this.ctx) return;
    const g = Number.isFinite(v) ? Math.max(0, Math.min(4, v)) : 0;
    s.gain.gain.setTargetAtTime(g, this.ctx.currentTime, 0.01);
  }

  detach(el: HTMLMediaElement): void {
    const s = this.strands.get(el);
    if (!s) return;
    try {
      s.gain.disconnect();
      s.source.disconnect();
    } catch {
      // Already gone.
    }
    this.strands.delete(el);
  }

  /** Follow the voice level; call often (every animation frame) while playing. */
  tick(): void {
    const ctx = this.ctx;
    if (!ctx || !this.analyser || !this.block || !this.duck) return;
    const d = this.ducking;
    let target = 1;
    if (d.enabled) {
      this.analyser.getFloatTimeDomainData(this.block);
      if (rmsDb(this.block) > VOICE_GATE_DB) target = dbToGain(d.duck_db);
    }
    if (target === this.duckTarget) return;
    const ms = target < this.duckTarget ? d.attack_ms : d.release_ms;
    this.duckTarget = target;
    this.duck.gain.setTargetAtTime(target, ctx.currentTime, Math.max(0.001, ms / 1000));
  }

  /** The music bus gain now (tests and the meter). */
  duckGain(): number {
    return this.duck?.gain.value ?? 1;
  }

  close(): void {
    for (const el of [...this.strands.keys()]) this.detach(el);
    void this.ctx?.close().catch(() => {});
    this.ctx = null;
    this.mainBus = this.musicBus = this.duck = this.voiceBus = null;
    this.analyser = null;
  }
}

/**
 * Normal mode's sound: the clip's tracks follow the preview's `<video>` (same time,
 * same rate, playing or not) through a `SoundGraph`, weighed by the Game/Mic sliders.
 * Without separate tracks the video's own sound is the Mix (Game slider).
 */
export class FollowSound {
  private graph = new SoundGraph();
  private strands: { el: HTMLMediaElement; name: string }[] = [];
  private volumes: Volumes = { game: 1, mic: 1 };
  private raf = 0;

  constructor(private video: HTMLVideoElement) {
    this.loop();
  }

  setTracks(tracks: { name: string; url: string }[]): void {
    for (const s of this.strands) {
      this.graph.detach(s.el);
      if (s.el !== this.video) {
        s.el.pause();
        s.el.removeAttribute('src');
        s.el.load();
      }
    }
    this.strands = [];
    this.video.muted = tracks.length > 0;
    if (tracks.length === 0) this.strands.push({ el: this.video, name: '' });
    for (const t of tracks) {
      const a = new Audio();
      a.crossOrigin = 'anonymous';
      a.preload = 'auto';
      a.src = t.url;
      this.strands.push({ el: a, name: t.name });
    }
    for (const s of this.strands) this.graph.attach(s.el, 'main', isVoice(s.name));
    this.apply();
  }

  setVolumes(v: Volumes): void {
    this.volumes = v;
    this.apply();
  }

  /** Call from the Play gesture (contexts start suspended). */
  resume(): void {
    this.graph.resume();
    this.apply();
  }

  private apply(): void {
    const separate = hasSeparateTracks(this.strands.map((s) => s.name));
    for (const s of this.strands) this.graph.setGain(s.el, trackWeight(s.name, separate, this.volumes) ?? 0);
  }

  private loop = (): void => {
    const v = this.video;
    for (const s of this.strands) {
      const el = s.el;
      if (el === v) continue;
      if (v.paused || v.seeking) {
        if (!el.paused) el.pause();
        if (!el.seeking && Math.abs(el.currentTime - v.currentTime) > 0.01) el.currentTime = v.currentTime;
        continue;
      }
      if (el.playbackRate !== v.playbackRate) el.playbackRate = v.playbackRate;
      if (el.paused) {
        el.currentTime = v.currentTime;
        void el.play().catch(() => {});
      } else if (!el.seeking && Math.abs(el.currentTime - v.currentTime) > 0.12) el.currentTime = v.currentTime;
    }
    this.raf = requestAnimationFrame(this.loop);
  };

  destroy(): void {
    cancelAnimationFrame(this.raf);
    this.setTracks([]);
    this.graph.close();
  }
}
