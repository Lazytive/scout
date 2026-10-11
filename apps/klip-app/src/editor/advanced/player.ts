/**
 * The Advanced preview (M9.5): a `Composition` drawn on a canvas with the export's
 * numbers (`fitted`/`place`/`visible`, opacity, the colour grade, text cues on top) and
 * heard through `SoundGraph`. Every layer that plays media gets its own elements (a
 * muted `<video>` for the picture, one `<audio>` per sound track), kept in step with a
 * wall-clock master: an element more than `RESYNC_S` off is moved. Layers starting
 * within `PREROLL_MS` are seeked ahead so cuts don't stall. While paused, each change
 * of time seeks the elements and draws when they arrive (the frame with the largest
 * presentation time ≤ the layer's source time, like the export).
 *
 * Without playable media (the mock, an unplayable codec) a video layer shows its
 * strip tile instead.
 */

import type { Source } from '../../lib/advanced';
import type { Composition, Layer } from '../../lib/compose';
import { layerActive, layerOpacityAt, layerRateAt, layerSourceMs, layerTransformAt, layerVolumeAt, visibleAt } from '../../lib/compose';
import type { Placement } from '../../lib/keyframes';
import { fitted, place, visible } from '../../lib/keyframes';
import type { Volumes } from '../../lib/timeline';
import { SoundGraph, hasSeparateTracks, isVoice, trackWeight } from './sound';

/** Layers starting this soon are seeked ahead (ms). */
export const PREROLL_MS = 1200;
/** An element further off than this while playing is moved (s). */
export const RESYNC_S = 0.12;
/** Unused elements are let go after this long (ms). */
const IDLE_MS = 4000;
/** A seek aims a tenth of a millisecond late: float error never shows the frame before. */
const EPS_S = 0.0001;

export interface StripRef {
  url: string;
  tiles: number;
  duration_ms: number;
}

/** What the preview can show and play for one source. */
export interface SourceMedia {
  /** A playable video (video layers) or sound (sound and music layers). */
  url: string | null;
  /** The clip's sound tracks, one file each (video layers). */
  tracks: { name: string; url: string }[];
  /** A picture (image layers). */
  image: string | null;
  /** The clip's frame size as probed (video layers). */
  width: number;
  height: number;
  /** Still frames when the video can't be played. */
  strip: StripRef | null;
}

export type Resolve = (s: Source) => SourceMedia | null;

/** A text cue as the engine drew it (`editor.text.render`), at `frame_width` x `frame_height`. */
export interface Cue {
  id: number;
  url: string;
  x: number;
  y: number;
  width: number;
  height: number;
  frame_width: number;
  frame_height: number;
  start_ms: number;
  end_ms: number;
}

interface Strand {
  el: HTMLMediaElement;
  name: string;
}

interface Slot {
  key: string;
  layer: Layer;
  video: HTMLVideoElement | null;
  sounds: Strand[];
  /** A seek waiting for the element's current one to finish (s). */
  pending: Map<HTMLMediaElement, number>;
  lastUsed: number;
}

export function sourceKey(s: Source): string {
  switch (s.kind) {
    case 'main':
      return 'main';
    case 'library':
      return `lib:${s.key}`;
    case 'file':
      return `file:${s.path}`;
    case 'image':
      return `image:${s.path}`;
  }
}

function slotKey(l: Layer): string {
  return `${l.track}:${l.at_ms}:${l.src_in_ms}:${sourceKey(l.source)}`;
}

export class Player {
  readonly sound = new SoundGraph();
  onTime: ((ms: number) => void) | null = null;
  onEnded: (() => void) | null = null;
  /** Called after each draw (tests). */
  onDraw: ((ms: number) => void) | null = null;

  private ctx: CanvasRenderingContext2D | null;
  private scratch: HTMLCanvasElement;
  private comp: Composition | null = null;
  private slots = new Map<string, Slot>();
  private images = new Map<string, HTMLImageElement>();
  private cues: (Cue & { img: HTMLImageElement })[] = [];
  private grade: string | null = null;
  private volumes: Volumes = { game: 1, mic: 1 };
  private t = 0;
  private playing = false;
  private startWall = 0;
  private startOut = 0;
  /** Shuttle speed while playing (L pressed again: 2x, 4x). */
  private rate = 1;
  private raf = 0;
  private drawQueued = false;
  private host: HTMLElement;

  constructor(
    private canvas: HTMLCanvasElement,
    private resolve: Resolve,
  ) {
    this.ctx = canvas.getContext('2d', { alpha: false });
    this.scratch = document.createElement('canvas');
    // Elements live in the document (some engines pause detached ones), out of sight.
    this.host = document.createElement('div');
    this.host.style.cssText = 'position:fixed;left:-10px;top:-10px;width:1px;height:1px;overflow:hidden;opacity:0;pointer-events:none';
    document.body.appendChild(this.host);
  }

  get time(): number {
    return this.t;
  }

  get isPlaying(): boolean {
    return this.playing;
  }

  setComposition(comp: Composition | null): void {
    this.comp = comp;
    if (comp) {
      this.volumes = comp.main_volumes;
      this.sound.setDucking(comp.ducking);
    }
    if (!this.playing) this.syncPaused();
    this.requestDraw();
  }

  /** The `<filter>` id of the colour grade, or `null` for none. */
  setGrade(filterId: string | null): void {
    this.grade = filterId;
    this.requestDraw();
  }

  setCues(cues: Cue[]): void {
    this.cues = cues.map((c) => {
      const img = new Image();
      img.decoding = 'async';
      img.onload = () => this.requestDraw();
      img.src = c.url;
      return { ...c, img };
    });
    this.requestDraw();
  }

  /** The canvas changed size (device pixels). */
  resize(width: number, height: number): void {
    const w = Math.max(2, Math.round(width));
    const h = Math.max(2, Math.round(height));
    if (this.canvas.width !== w || this.canvas.height !== h) {
      this.canvas.width = w;
      this.canvas.height = h;
    }
    this.requestDraw();
  }

  /** The media changed (a proxy is ready, a file was allowed): start over with fresh elements. */
  reload(): void {
    for (const s of this.slots.values()) this.release(s);
    this.slots.clear();
    this.images.clear();
    if (!this.playing) this.syncPaused();
    this.requestDraw();
  }

  seek(ms: number): void {
    const end = Math.max(0, (this.comp?.duration_ms ?? 0) - 1);
    this.t = Math.min(Math.max(0, Math.round(ms)), end);
    if (this.playing) {
      this.startWall = performance.now();
      this.startOut = this.t;
      for (const s of this.slots.values()) for (const el of this.elements(s)) el.pause();
      this.step();
    } else {
      this.syncPaused();
      this.requestDraw();
    }
  }

  /** Play at `r` times realtime (1, 2, 4). */
  setRate(r: number): void {
    const rate = Math.min(4, Math.max(0.25, r));
    if (this.playing) {
      this.startOut = this.t;
      this.startWall = performance.now();
    }
    this.rate = rate;
  }

  play(): void {
    if (this.playing || !this.comp) return;
    if (this.t >= this.comp.duration_ms - 1) this.t = 0;
    this.sound.resume();
    this.playing = true;
    this.startWall = performance.now();
    this.startOut = this.t;
    this.loop();
  }

  pause(): void {
    if (!this.playing) return;
    this.playing = false;
    cancelAnimationFrame(this.raf);
    for (const s of this.slots.values()) {
      for (const el of this.elements(s)) el.pause();
      for (const st of s.sounds) this.sound.setGain(st.el, 0);
    }
    this.syncPaused();
    this.requestDraw();
  }

  destroy(): void {
    this.playing = false;
    cancelAnimationFrame(this.raf);
    for (const s of this.slots.values()) this.release(s);
    this.slots.clear();
    this.sound.close();
    this.host.remove();
  }

  /** Draw the picture at `ms` now, waiting (up to `timeoutMs`) for every visible video to arrive there. */
  async frameAt(ms: number, timeoutMs = 5000): Promise<void> {
    this.pause();
    this.seek(ms);
    const deadline = performance.now() + timeoutMs;
    const ready = () =>
      [...this.slots.values()].every((s) => {
        if (!layerActive(s.layer, this.t) || !s.video) return true;
        const v = s.video;
        const want = (layerSourceMs(s.layer, this.t) ?? 0) / 1000 + EPS_S;
        return !v.seeking && v.readyState >= 2 && Math.abs(v.currentTime - want) < 0.002 && !s.pending.has(v);
      });
    while (!ready() && performance.now() < deadline) await new Promise((r) => setTimeout(r, 20));
    await Promise.all([...this.images.values()].map((i) => (i.complete ? null : i.decode().catch(() => {}))));
    this.draw();
  }

  /** Where layer `l` lands on the canvas at the current time (canvas pixels), if known. */
  placementOf(l: Layer): Placement | null {
    const comp = this.comp;
    if (!comp || l.kind === 'black') return null;
    const m = this.resolve(l.source);
    if (!m) return null;
    let native: [number, number] = [m.width, m.height];
    if (l.kind === 'image') {
      const img = m.image ? this.image(m.image) : null;
      if (!img || img.naturalWidth === 0) return null;
      native = [img.naturalWidth, img.naturalHeight];
    }
    const W = this.canvas.width;
    const H = this.canvas.height;
    const fit = fitted(l.kind, l.source.kind === 'main', native, comp.crop.width, [W, H]);
    return place(layerTransformAt(l, this.t), fit[0], fit[1], W, H);
  }

  // ---- elements ----

  private elements(s: Slot): HTMLMediaElement[] {
    const out: HTMLMediaElement[] = s.sounds.map((x) => x.el);
    if (s.video && !s.sounds.some((x) => x.el === s.video)) out.push(s.video);
    return out;
  }

  private media(el: HTMLMediaElement, url: string, slot: Slot): void {
    el.crossOrigin = 'anonymous';
    el.preload = 'auto';
    el.src = url;
    el.addEventListener('seeked', () => {
      const next = slot.pending.get(el);
      if (next !== undefined) {
        slot.pending.delete(el);
        if (Math.abs(el.currentTime - next) > 0.0005) el.currentTime = next;
      }
      this.requestDraw();
    });
    el.addEventListener('loadeddata', () => this.requestDraw());
    this.host.appendChild(el);
  }

  private ensure(l: Layer): Slot | null {
    const key = slotKey(l);
    let s = this.slots.get(key);
    if (s) {
      s.layer = l;
      s.lastUsed = performance.now();
      return s;
    }
    // Pictures and a dip's black have no media element.
    if (l.kind === 'image' || l.kind === 'black') return null;
    const m = this.resolve(l.source);
    if (!m) return null;
    s = { key, layer: l, video: null, sounds: [], pending: new Map(), lastUsed: performance.now() };
    if (l.kind === 'video') {
      if (m.url) {
        const v = document.createElement('video');
        v.playsInline = true;
        v.muted = m.tracks.length > 0;
        this.media(v, m.url, s);
        s.video = v;
        // No separate tracks: the video's own sound is the Mix.
        if (m.tracks.length === 0) s.sounds.push({ el: v, name: '' });
      }
      for (const t of m.tracks) {
        const a = new Audio();
        this.media(a, t.url, s);
        s.sounds.push({ el: a, name: t.name });
      }
    } else if (m.url) {
      const a = new Audio();
      this.media(a, m.url, s);
      s.sounds.push({ el: a, name: l.kind === 'audio' ? 'Voice' : 'Music' });
    }
    for (const st of s.sounds) {
      const voice = l.kind === 'audio' || (l.kind === 'video' && isVoice(st.name));
      this.sound.attach(st.el, l.kind === 'music' ? 'music' : 'main', voice);
    }
    this.slots.set(key, s);
    return s;
  }

  private release(s: Slot): void {
    for (const el of this.elements(s)) {
      el.pause();
      this.sound.detach(el);
      el.removeAttribute('src');
      el.load();
      el.remove();
    }
  }

  private image(url: string): HTMLImageElement {
    let img = this.images.get(url);
    if (!img) {
      img = new Image();
      img.crossOrigin = 'anonymous';
      img.decoding = 'async';
      img.onload = () => this.requestDraw();
      img.src = url;
      this.images.set(url, img);
    }
    return img;
  }

  /** The gain of one sound strand of `l` at `t`. */
  private gainOf(l: Layer, s: Slot, st: Strand, t: number): number {
    if (l.muted) return 0;
    const vol = layerVolumeAt(l, t);
    if (l.kind !== 'video') return vol;
    const vols = l.source.kind === 'main' ? this.volumes : { game: 1, mic: 1 };
    const w = trackWeight(st.name, hasSeparateTracks(s.sounds.map((x) => x.name)), vols);
    return w === null ? 0 : w * vol;
  }

  private target(l: Layer, t: number): number {
    return (layerSourceMs(l, t) ?? l.src_in_ms) / 1000 + EPS_S;
  }

  /** Paused: every active layer's elements at the exact time, silent. */
  private syncPaused(): void {
    const comp = this.comp;
    if (!comp) return;
    const t = this.t;
    for (const l of comp.layers) {
      if (!layerActive(l, t) || l.kind === 'image' || l.kind === 'black') continue;
      const s = this.ensure(l);
      if (!s) continue;
      const want = this.target(l, t);
      for (const el of this.elements(s)) {
        if (!el.paused) el.pause();
        if (el.seeking) s.pending.set(el, want);
        else if (Math.abs(el.currentTime - want) > 0.0005) el.currentTime = want;
      }
      for (const st of s.sounds) this.sound.setGain(st.el, 0);
    }
    this.collect();
  }

  /** Let go of elements unused for a while. */
  private collect(): void {
    const now = performance.now();
    for (const [k, s] of this.slots) {
      if (now - s.lastUsed > IDLE_MS && !(this.comp && layerActive(s.layer, this.t))) {
        this.release(s);
        this.slots.delete(k);
      }
    }
  }

  // ---- playing ----

  private loop = (): void => {
    if (!this.playing) return;
    this.step();
    if (this.playing) this.raf = requestAnimationFrame(this.loop);
  };

  private step(): void {
    const comp = this.comp;
    if (!comp) return;
    const t = Math.round(this.startOut + (performance.now() - this.startWall) * this.rate);
    if (t >= comp.duration_ms) {
      this.t = Math.max(0, comp.duration_ms - 1);
      this.onTime?.(this.t);
      this.pause();
      this.onEnded?.();
      return;
    }
    this.t = t;
    const used = new Set<string>();
    for (const l of comp.layers) {
      if (l.kind === 'image' || l.kind === 'black') continue;
      const active = layerActive(l, t);
      const soon = !active && l.at_ms > t && l.at_ms - t <= PREROLL_MS;
      if (!active && !soon) continue;
      const s = this.ensure(l);
      if (!s) continue;
      used.add(s.key);
      if (active) {
        const want = this.target(l, t);
        const rate = Math.min(16, layerRateAt(l, t) * this.rate);
        for (const el of this.elements(s)) {
          if (el.playbackRate !== rate) el.playbackRate = rate;
          if (el.paused) {
            if (Math.abs(el.currentTime - want) > RESYNC_S / 2) el.currentTime = want;
            void el.play().then(
              () => delete el.dataset.playError,
              (e: unknown) => (el.dataset.playError = e instanceof Error ? e.name : 'refused'),
            );
          } else if (!el.seeking && Math.abs(el.currentTime - want) > RESYNC_S) el.currentTime = want;
        }
        for (const st of s.sounds) this.sound.setGain(st.el, this.gainOf(l, s, st, t));
      } else {
        const start = l.src_in_ms / 1000 + EPS_S;
        for (const el of this.elements(s)) {
          if (!el.paused) el.pause();
          if (!el.seeking && Math.abs(el.currentTime - start) > 0.05) el.currentTime = start;
        }
        for (const st of s.sounds) this.sound.setGain(st.el, 0);
      }
    }
    for (const s of this.slots.values()) {
      if (used.has(s.key)) continue;
      for (const el of this.elements(s)) if (!el.paused) el.pause();
      for (const st of s.sounds) this.sound.setGain(st.el, 0);
    }
    this.sound.tick();
    this.collect();
    this.draw();
    this.onTime?.(t);
  }

  // ---- drawing ----

  private requestDraw(): void {
    if (this.drawQueued || this.playing) return;
    this.drawQueued = true;
    requestAnimationFrame(() => {
      this.drawQueued = false;
      if (!this.playing) this.draw();
    });
  }

  /** The picture at the current time. */
  draw(): void {
    const comp = this.comp;
    const out = this.ctx;
    if (!comp || !out) return;
    const W = this.canvas.width;
    const H = this.canvas.height;
    const graded = this.grade !== null;
    if (graded && (this.scratch.width !== W || this.scratch.height !== H)) {
      this.scratch.width = W;
      this.scratch.height = H;
    }
    const ctx = graded ? this.scratch.getContext('2d', { alpha: false }) : out;
    if (!ctx) return;
    ctx.globalAlpha = 1;
    ctx.fillStyle = '#000';
    ctx.fillRect(0, 0, W, H);
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = 'high';
    const t = this.t;
    const crop = comp.crop;
    for (const l of visibleAt(comp, t)) {
      if (l.kind === 'black') {
        // A dip's black: the whole frame at its opacity (the export does the same).
        ctx.globalAlpha = layerOpacityAt(l, t);
        if (ctx.globalAlpha > 0) {
          ctx.fillStyle = '#000';
          ctx.fillRect(0, 0, W, H);
        }
        continue;
      }
      const m = this.resolve(l.source);
      if (!m) continue;
      const main = l.source.kind === 'main';
      let pic: CanvasImageSource;
      let native: [number, number];
      // The picture's own pixel rectangle that `visible` maps from, and its scale to the element.
      let picRect: { x: number; y: number; w: number; h: number };
      let kx = 1;
      let ky = 1;
      if (l.kind === 'image') {
        const img = m.image ? this.image(m.image) : null;
        if (!img || !img.complete || img.naturalWidth === 0) continue;
        pic = img;
        native = [img.naturalWidth, img.naturalHeight];
        picRect = { x: 0, y: 0, w: native[0], h: native[1] };
      } else {
        native = [m.width, m.height];
        picRect = main ? { x: crop.x, y: crop.y, w: crop.width, h: crop.height } : { x: 0, y: 0, w: m.width, h: m.height };
        const s = this.slots.get(slotKey(l));
        const v = s?.video;
        if (v && v.readyState >= 2 && v.videoWidth > 0) {
          pic = v;
          kx = v.videoWidth / Math.max(1, m.width);
          ky = v.videoHeight / Math.max(1, m.height);
        } else if (m.strip) {
          const strip = this.image(m.strip.url);
          if (!strip.complete || strip.naturalWidth === 0) continue;
          const tiles = Math.max(1, m.strip.tiles);
          const src = layerSourceMs(l, t) ?? l.src_in_ms;
          const idx = Math.min(tiles - 1, Math.max(0, Math.floor((src / Math.max(1, m.strip.duration_ms)) * tiles)));
          const tileH = strip.naturalHeight / tiles;
          pic = strip;
          kx = strip.naturalWidth / Math.max(1, m.width);
          ky = tileH / Math.max(1, m.height);
          picRect = { ...picRect, y: picRect.y + (idx * tileH) / ky };
        } else continue;
      }
      const fit = fitted(l.kind, main, native, crop.width, [W, H]);
      const p = place(layerTransformAt(l, t), fit[0], fit[1], W, H);
      const vis = visible(p, picRect.w, picRect.h, W, H);
      if (!vis) continue;
      const [s, d] = vis;
      ctx.globalAlpha = layerOpacityAt(l, t);
      if (ctx.globalAlpha <= 0) continue;
      ctx.drawImage(pic, (picRect.x + s.x) * kx, (picRect.y + s.y) * ky, s.width * kx, s.height * ky, d.x, d.y, d.width, d.height);
    }
    ctx.globalAlpha = 1;
    if (graded) {
      out.filter = `url(#${this.grade})`;
      out.drawImage(this.scratch, 0, 0);
      out.filter = 'none';
    }
    for (const c of this.cues) {
      if (t < c.start_ms || t >= c.end_ms || !c.img.complete || c.img.naturalWidth === 0) continue;
      const sx = W / Math.max(1, c.frame_width);
      const sy = H / Math.max(1, c.frame_height);
      out.drawImage(c.img, c.x * sx, c.y * sy, c.width * sx, c.height * sy);
    }
    this.onDraw?.(t);
  }
}
