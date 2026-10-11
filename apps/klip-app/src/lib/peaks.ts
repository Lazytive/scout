// Waveforms for the timeline (M9.7.3): a sound file decoded once into peaks (the largest
// sample per 10 ms over all channels), cached by URL; `peaksPath` draws a stretch of them
// as an SVG path (mirrored around the middle) in a `0 0 buckets 1` box, on a loudness
// scale like Premiere's logarithmic waveforms (−60 dB … 0 dB fills the height: a quiet
// capture peaking at −41 dB still fills a third).

/** The quietest level a waveform still shows, dBFS. */
export const FLOOR_DB = -60;

/** A peak (0…1) as a share of the waveform's height, on the loudness scale. */
export function loudness(peak: number): number {
  if (!(peak > 0)) return 0;
  return Math.min(1, Math.max(0, (20 * Math.log10(peak) - FLOOR_DB) / -FLOOR_DB));
}

/** Peaks per second of sound. */
export const PEAKS_PER_SEC = 100;
/** Files bigger than this aren't decoded for a waveform (bytes). */
const MAX_BYTES = 96 * 1024 * 1024;
const MAX_CACHED = 32;

const cache = new Map<string, Promise<Float32Array | null>>();

export function loadPeaks(url: string): Promise<Float32Array | null> {
  let p = cache.get(url);
  if (!p) {
    p = decode(url);
    cache.set(url, p);
    if (cache.size > MAX_CACHED) {
      const oldest = cache.keys().next().value;
      if (oldest !== undefined) cache.delete(oldest);
    }
  }
  return p;
}

async function decode(url: string): Promise<Float32Array | null> {
  try {
    const res = await fetch(url);
    if (!res.ok) return null;
    if (Number(res.headers.get('content-length') ?? 0) > MAX_BYTES) return null;
    const buf = await res.arrayBuffer();
    if (buf.byteLength > MAX_BYTES || buf.byteLength === 0) return null;
    const ctx = new OfflineAudioContext(1, 1, 44_100);
    const audio = await ctx.decodeAudioData(buf);
    return peaksOf(
      Array.from({ length: audio.numberOfChannels }, (_, i) => audio.getChannelData(i)),
      audio.sampleRate,
    );
  } catch {
    return null;
  }
}

/** The largest |sample| per 1/PEAKS_PER_SEC s over all channels. */
export function peaksOf(channels: Float32Array[], sampleRate: number): Float32Array {
  const per = Math.max(1, Math.round(sampleRate / PEAKS_PER_SEC));
  const len = channels[0]?.length ?? 0;
  const out = new Float32Array(Math.ceil(len / per));
  for (const d of channels) {
    for (let i = 0; i < out.length; i++) {
      let m = out[i] ?? 0;
      const end = Math.min(len, (i + 1) * per);
      for (let j = i * per; j < end; j++) {
        const v = Math.abs(d[j] ?? 0);
        if (v > m) m = v;
      }
      out[i] = m;
    }
  }
  return out;
}

/** Several sounds' peaks as one (the louder at each moment); null when there are none. */
export function mergePeaks(all: Float32Array[]): Float32Array | null {
  if (all.length === 0) return null;
  if (all.length === 1) return all[0]!;
  const out = new Float32Array(Math.max(...all.map((p) => p.length)));
  for (const p of all) for (let i = 0; i < p.length; i++) out[i] = Math.max(out[i] ?? 0, p[i] ?? 0);
  return out;
}

/** The peaks between source times `fromMs`…`toMs` as `buckets` columns, as an SVG path. */
export function peaksPath(peaks: Float32Array, fromMs: number, toMs: number, buckets: number): string {
  const n = Math.max(1, Math.min(4000, Math.round(buckets)));
  const a = Math.max(0, (fromMs / 1000) * PEAKS_PER_SEC);
  const b = Math.max(a, (toMs / 1000) * PEAKS_PER_SEC);
  const tops: number[] = [];
  for (let i = 0; i < n; i++) {
    const s = Math.floor(a + ((b - a) * i) / n);
    const e = Math.max(s + 1, Math.floor(a + ((b - a) * (i + 1)) / n));
    let m = 0;
    for (let j = s; j < e && j < peaks.length; j++) m = Math.max(m, peaks[j] ?? 0);
    tops.push(loudness(m));
  }
  const r = (v: number) => Math.round(v * 1000) / 1000;
  let d = `M0 0.5`;
  tops.forEach((m, i) => (d += `L${i} ${r(0.5 - m / 2)}L${i + 1} ${r(0.5 - m / 2)}`));
  for (let i = n - 1; i >= 0; i--) d += `L${i + 1} ${r(0.5 + (tops[i] ?? 0) / 2)}L${i} ${r(0.5 + (tops[i] ?? 0) / 2)}`;
  return `${d}Z`;
}
