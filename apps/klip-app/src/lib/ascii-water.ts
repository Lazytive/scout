/*
 * ASCII water (an animation experiment, not for the release; its switch is Settings → Debug,
 * tester builds and the browser mock): a grid of faint monospace glyphs whose density
 * follows moving light on the floor of a pool. The pointer and random drops ripple the
 * surface, and the ripples bend the light.
 *
 * Adapted from the user's own page Claude/ascii-water (ripple simulation, glyph atlas,
 * draw pass, patch mask). Its light pattern was a Shadertoy port (CC BY-NC-SA by
 * default), which Scout can't take (AGENTS.md rule 5), so the light here is Scout's own:
 * the borders of slowly drifting cells (two Voronoi layers), warped.
 *
 * Three WebGL2 passes; only the last runs per screen pixel:
 *   1. ripples: the two-buffer wave equation on a cell grid (2 rows per text row), 60 steps/s
 *   2. light: one brightness per glyph cell -> a glyph index (R8 texture)
 *   3. glyphs: each pixel reads its cell's glyph index and the glyph atlas
 * It animates only while the window has focus and is visible (so it rests while a game
 * runs), and with reduced motion it draws one still frame.
 */

export interface WaterOptions {
  fontSize: number; // glyph size, CSS px
  lineHeight: number; // row height, CSS px
  ramp: string; // sparse -> dense; index 0 is "empty"
  alpha: [number, number, number]; // opacity of the faint / mid / bright glyph layers
  flow: number; // drift speed
  zoom: number; // light cells per CSS px (smaller = bigger cells)
  sharpness: number; // higher = thinner light lines
  cutoff: [number, number]; // brightness range spread over the ramp (below = empty)
  patchiness: number; // 0 = even coverage, 1 = large dark regions
  damping: number; // ripple fade per step
  rain: number; // chance per step (60/s) of a random drop
  refraction: number; // CSS px the light shifts per unit of ripple slope
  shine: number; // ripple highlight strength
}

export const WATER_DEFAULTS: WaterOptions = {
  fontSize: 11,
  lineHeight: 13,
  ramp: ' .:-=+*#%@',
  alpha: [0.07, 0.12, 0.19],
  flow: 0.45,
  zoom: 0.008,
  sharpness: 5,
  cutoff: [0.06, 0.75],
  patchiness: 0.6,
  damping: 0.985,
  rain: 0.03,
  refraction: 60,
  shine: 0.8,
};

const VERT = `#version 300 es
void main() {  // one triangle that covers the whole target
  gl_Position = vec4(vec2(gl_VertexID & 1, gl_VertexID >> 1) * 4.0 - 1.0, 0.0, 1.0);
}`;

// Wave equation, two-buffer trick: next = (sum of 4 neighbours) / 2 - previous.
const SIM = `#version 300 es
precision highp float;
uniform sampler2D state;  // r = height now, g = height one step ago
uniform vec4 drops[16];   // x, y (sim cells), radius, strength
uniform int dropCount;
uniform float damping;
out vec4 o;
float h(ivec2 p) {
  return texelFetch(state, clamp(p, ivec2(0), textureSize(state, 0) - 1), 0).r;
}
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  vec2 here = texelFetch(state, p, 0).rg;
  float next = ((h(p + ivec2(1, 0)) + h(p - ivec2(1, 0)) +
                 h(p + ivec2(0, 1)) + h(p - ivec2(0, 1))) * 0.5 - here.g) * damping;
  for (int i = 0; i < 16; i++) {
    if (i >= dropCount) break;
    float d = length(gl_FragCoord.xy - drops[i].xy) / drops[i].z;
    if (d < 1.0) next += drops[i].w * (1.0 + cos(d * 3.14159265)) * 0.5;
  }
  o = vec4(next, here.r, 0.0, 0.0);
}`;

// One fragment per glyph cell: how bright the pool floor is there, as a glyph index.
const LIGHT = `#version 300 es
precision highp float;
uniform sampler2D sim;
uniform vec2 cellCss, cutoff;  // glyph cell in CSS px; brightness range
uniform float glyphs, t, zoom, sharpness, patchiness, refraction, shine;
out vec4 o;

float rnd(ivec2 c, uint salt) {  // integer hash -> 0..1 (lowbias32 finaliser)
  uint n = uint(c.x + 32768) * 73856093u ^ uint(c.y + 32768) * 19349663u ^ salt * 83492791u;
  n ^= n >> 16; n *= 0x7feb352du; n ^= n >> 15; n *= 0x846ca68bu; n ^= n >> 16;
  return float(n) * (1.0 / 4294967295.0);
}

// Distance to the nearest border between drifting cells: 0 on a border.
float border(vec2 p, float time, uint salt) {
  vec2 c = floor(p);
  float f1 = 9.0, f2 = 9.0;
  for (int y = -1; y <= 1; y++) {
    for (int x = -1; x <= 1; x++) {
      ivec2 g = ivec2(c) + ivec2(x, y);
      vec2 r = vec2(rnd(g, salt), rnd(g, salt + 7u));
      vec2 q = vec2(g) + 0.5 + 0.4 * sin(time * (0.5 + r.yx) + 6.2831853 * r);
      float d = length(q - p);
      if (d < f1) { f2 = f1; f1 = d; } else if (d < f2) { f2 = d; }
    }
  }
  return f2 - f1;
}

// Pool-floor light: bright where the borders of two cell layers run, brightest where
// they cross. Roughly 0..1.
float light(vec2 p) {
  float w = 1.5 / sharpness;
  float a = 1.0 - smoothstep(0.0, w, border(p, t, 1u));
  float b = 1.0 - smoothstep(0.0, w * 1.4, border(p * 1.7 + vec2(5.3, 1.9), t * 1.3, 3u));
  return a * a * 0.75 + b * b * 0.35 + a * b * 0.4;
}

float mask(vec2 x) {             // slow, large light/dark regions (x in CSS px)
  float m = sin(x.x * 0.0023 + t * 0.07 + sin(x.y * 0.0031 - t * 0.05) * 1.7)
          * sin(x.y * 0.0029 - t * 0.06 + sin(x.x * 0.0017 + t * 0.04) * 1.3);
  return 1.0 - patchiness * (0.5 - 0.5 * m);
}

float height(ivec2 p) {
  return texelFetch(sim, clamp(p, ivec2(0), textureSize(sim, 0) - 1), 0).r;
}

void main() {
  vec2 cellPos = floor(gl_FragCoord.xy);
  ivec2 s = ivec2(cellPos.x, cellPos.y * 2.0);   // 2 ripple rows per text row
  vec2 slope = vec2(
    (height(s + ivec2(1, 0)) - height(s - ivec2(1, 0)) +
     height(s + ivec2(1, 1)) - height(s + ivec2(-1, 1))) * 0.5,
    height(s + ivec2(0, 2)) - height(s - ivec2(0, 1)));

  vec2 center = (cellPos + 0.5) * cellCss;       // CSS px
  vec2 p = (center + slope * refraction) * zoom;
  p += 0.35 * vec2(sin(p.y * 0.9 + t * 0.31) + 0.5 * sin(p.y * 2.1 - p.x * 0.7),
                   sin(p.x * 0.8 - t * 0.27) + 0.5 * sin(p.x * 1.9 + p.y * 0.6));

  float b = smoothstep(cutoff.x, cutoff.y, light(p) * mask(center))
          + (slope.y - slope.x) * shine;
  o = vec4(floor(clamp(b, 0.0, 1.0) * (glyphs - 1.0) + 0.5) / 255.0, 0.0, 0.0, 1.0);
}`;

const DRAW = `#version 300 es
precision highp float;
uniform sampler2D light, atlas;
uniform vec2 cell;               // glyph cell in device px
uniform float glyphs;
uniform vec3 rgb, alpha;
out vec4 o;
void main() {
  vec2 cellPos = floor(gl_FragCoord.xy / cell);
  float k = floor(texelFetch(light, ivec2(cellPos), 0).r * 255.0 + 0.5);
  if (k < 1.0) { o = vec4(0.0); return; }
  vec2 px = floor(mod(gl_FragCoord.xy, cell));   // pixel inside the cell
  float ink = texelFetch(atlas, ivec2(k * cell.x + px.x, cell.y - 1.0 - px.y), 0).a;
  float a = alpha[int(min(2.0, floor((k - 1.0) * 3.0 / (glyphs - 1.0))))] * ink;
  o = vec4(rgb * a, a);
}`;

type Uniforms = Record<string, WebGLUniformLocation | null>;

/** A CSS colour as 0..1 RGB (via a 1×1 canvas, so any CSS colour syntax works). */
function cssRgb(color: string): [number, number, number] {
  const c = document.createElement('canvas');
  c.width = c.height = 1;
  const g = c.getContext('2d', { willReadFrequently: true });
  if (!g) return [0.8, 0.8, 0.8];
  g.fillStyle = color;
  g.fillRect(0, 0, 1, 1);
  const [r = 204, gr = 204, b = 204] = g.getImageData(0, 0, 1, 1).data;
  return [r / 255, gr / 255, b / 255];
}

/**
 * Starts the water on `canvas` (transparent; it fills its CSS box). The pointer over
 * `input` stirs it. Returns null without WebGL2 + float render targets.
 */
export function startAsciiWater(
  canvas: HTMLCanvasElement,
  input: HTMLElement,
  opts: Partial<WaterOptions> = {},
): { stop(): void } | null {
  const C: WaterOptions = { ...WATER_DEFAULTS, ...opts };
  const ctx = canvas.getContext('webgl2', { antialias: false, depth: false });
  if (!ctx || !ctx.getExtension('EXT_color_buffer_float')) {
    console.warn('ASCII water needs WebGL2 with float render targets');
    return null;
  }
  const gl: WebGL2RenderingContext = ctx;
  let raf = 0,
    last = 0,
    acc = 0,
    running = false,
    lost = false;

  function program(frag: string): { p: WebGLProgram; u: Uniforms } {
    const p = gl.createProgram();
    for (const [type, src] of [
      [gl.VERTEX_SHADER, VERT],
      [gl.FRAGMENT_SHADER, frag],
    ] as const) {
      const s = gl.createShader(type);
      if (!s) throw new Error('no shader');
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s) ?? 'shader');
      gl.attachShader(p, s);
    }
    gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(p) ?? 'link');
    const u: Uniforms = {};
    const n = gl.getProgramParameter(p, gl.ACTIVE_UNIFORMS) as number;
    for (let i = 0; i < n; i++) {
      const name = gl.getActiveUniform(p, i)?.name.replace('[0]', '');
      if (name) u[name] = gl.getUniformLocation(p, name);
    }
    return { p, u };
  }

  function texture(
    w: number,
    h: number,
    internal: number,
    format: number,
    type: number,
    src: TexImageSource | null,
  ): WebGLTexture {
    const tex = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, tex);
    if (src) gl.texImage2D(gl.TEXTURE_2D, 0, internal, w, h, 0, format, type, src);
    else gl.texImage2D(gl.TEXTURE_2D, 0, internal, w, h, 0, format, type, null);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    return tex;
  }

  function target(tex: WebGLTexture): WebGLFramebuffer {
    const fb = gl.createFramebuffer();
    gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
    gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, tex, 0);
    return fb;
  }

  let simProg: ReturnType<typeof program>, lightProg: ReturnType<typeof program>, drawProg: ReturnType<typeof program>;
  try {
    simProg = program(SIM);
    lightProg = program(LIGHT);
    drawProg = program(DRAW);
  } catch (e) {
    console.warn('ASCII water: shader failed', e);
    return null;
  }

  const glyphCount = [...C.ramp].length;
  let dpr = 1,
    cw = 1,
    lh = 1,
    cols = 1,
    rows = 1,
    simW = 1,
    simH = 1;
  let atlas: WebGLTexture | null = null;
  let light: { tex: WebGLTexture; fb: WebGLFramebuffer } | null = null;
  let sims: { tex: WebGLTexture; fb: WebGLFramebuffer }[] = [];
  let rgb: [number, number, number] = [0.8, 0.8, 0.8];

  const readColor = () => {
    rgb = cssRgb(getComputedStyle(canvas).getPropertyValue('--text').trim() || '#d2d2d2');
  };

  function resize() {
    dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.max(1, Math.round(canvas.clientWidth * dpr));
    canvas.height = Math.max(1, Math.round(canvas.clientHeight * dpr));

    // Glyph atlas: the ramp drawn white on transparent, one whole-pixel cell per glyph.
    const font = `${C.fontSize * dpr}px ui-monospace, "Cascadia Mono", Consolas, monospace`;
    const tc = document.createElement('canvas');
    const g = tc.getContext('2d');
    if (!g) return;
    g.font = font;
    cw = Math.max(1, Math.round(g.measureText('M').width));
    lh = Math.max(1, Math.round(C.lineHeight * dpr));
    tc.width = cw * glyphCount;
    tc.height = lh;
    g.font = font;
    g.textAlign = 'center';
    g.textBaseline = 'middle';
    g.fillStyle = '#fff';
    [...C.ramp].forEach((ch, i) => g.fillText(ch, (i + 0.5) * cw, lh / 2));
    if (atlas) gl.deleteTexture(atlas);
    atlas = texture(tc.width, tc.height, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, tc);

    // Light: one texel per glyph cell. Ripples: one column per glyph, two rows per text
    // row, so their cells are about square.
    cols = Math.ceil(canvas.width / cw);
    rows = Math.ceil(canvas.height / lh);
    simW = cols;
    simH = rows * 2;
    if (light) {
      gl.deleteTexture(light.tex);
      gl.deleteFramebuffer(light.fb);
    }
    const lt = texture(cols, rows, gl.R8, gl.RED, gl.UNSIGNED_BYTE, null);
    light = { tex: lt, fb: target(lt) };
    for (const s of sims) {
      gl.deleteTexture(s.tex);
      gl.deleteFramebuffer(s.fb);
    }
    sims = [0, 1].map(() => {
      const tex = texture(simW, simH, gl.RG16F, gl.RG, gl.HALF_FLOAT, null);
      return { tex, fb: target(tex) };
    });
    readColor();
    if (!running) render();
  }

  // Drops wait here as x, y, radius, strength (sim cells) until the next step.
  const pending: number[] = [];
  const dropBuf = new Float32Array(64);
  const drop = (x: number, y: number, radius: number, strength: number) => {
    if (pending.length < 256) pending.push(x, y, radius, strength);
  };

  function step() {
    if (Math.random() < C.rain) {
      drop(Math.random() * simW, Math.random() * simH, 2 + Math.random() * 2, 0.5 + Math.random() * 0.8);
    }
    const [from, to] = sims;
    if (!from || !to) return;
    const batch = pending.splice(0, 64);
    dropBuf.fill(0);
    dropBuf.set(batch);
    gl.bindFramebuffer(gl.FRAMEBUFFER, to.fb);
    gl.viewport(0, 0, simW, simH);
    gl.useProgram(simProg.p);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, from.tex);
    gl.uniform1i(simProg.u.state ?? null, 0);
    gl.uniform4fv(simProg.u.drops ?? null, dropBuf);
    gl.uniform1i(simProg.u.dropCount ?? null, batch.length / 4);
    gl.uniform1f(simProg.u.damping ?? null, C.damping);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    sims.reverse();
  }

  let clock = 23; // pattern time; only moves while animating, so a pause doesn't jump

  function render() {
    const sim = sims[0];
    if (!sim || !light || !atlas) return;
    // Pass 2: light per glyph cell.
    let u = lightProg.u;
    gl.bindFramebuffer(gl.FRAMEBUFFER, light.fb);
    gl.viewport(0, 0, cols, rows);
    gl.useProgram(lightProg.p);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, sim.tex);
    gl.uniform1i(u.sim ?? null, 0);
    gl.uniform2f(u.cellCss ?? null, cw / dpr, lh / dpr);
    gl.uniform2f(u.cutoff ?? null, C.cutoff[0], C.cutoff[1]);
    gl.uniform1f(u.glyphs ?? null, glyphCount);
    gl.uniform1f(u.t ?? null, clock);
    gl.uniform1f(u.zoom ?? null, C.zoom);
    gl.uniform1f(u.sharpness ?? null, C.sharpness);
    gl.uniform1f(u.patchiness ?? null, C.patchiness);
    gl.uniform1f(u.refraction ?? null, C.refraction);
    gl.uniform1f(u.shine ?? null, C.shine);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
    // Pass 3: glyphs per pixel.
    u = drawProg.u;
    gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.useProgram(drawProg.p);
    gl.activeTexture(gl.TEXTURE0);
    gl.bindTexture(gl.TEXTURE_2D, light.tex);
    gl.activeTexture(gl.TEXTURE1);
    gl.bindTexture(gl.TEXTURE_2D, atlas);
    gl.uniform1i(u.light ?? null, 0);
    gl.uniform1i(u.atlas ?? null, 1);
    gl.uniform2f(u.cell ?? null, cw, lh);
    gl.uniform1f(u.glyphs ?? null, glyphCount);
    gl.uniform3f(u.rgb ?? null, rgb[0], rgb[1], rgb[2]);
    gl.uniform3f(u.alpha ?? null, C.alpha[0], C.alpha[1], C.alpha[2]);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  // Pointer: a trail of small drops while moving, a big drop on press.
  const ac = new AbortController();
  let prev: [number, number] | null = null;
  let prevTime = 0;
  const toSim = (e: PointerEvent): [number, number] => {
    const r = canvas.getBoundingClientRect();
    return [((e.clientX - r.left) * dpr) / cw, ((r.bottom - e.clientY) * dpr * 2) / lh];
  };
  input.addEventListener(
    'pointermove',
    (e) => {
      if (!running) return;
      const [x, y] = toSim(e);
      if (prev && e.timeStamp - prevTime < 120) {
        const n = Math.min(24, Math.ceil(Math.hypot(x - prev[0], y - prev[1]) / 2));
        for (let s = 1; s <= n; s++) {
          drop(prev[0] + ((x - prev[0]) * s) / n, prev[1] + ((y - prev[1]) * s) / n, 2.5, 0.22);
        }
      }
      prev = [x, y];
      prevTime = e.timeStamp;
    },
    { signal: ac.signal },
  );
  input.addEventListener(
    'pointerdown',
    (e) => {
      if (!running) return;
      const [x, y] = toSim(e);
      drop(x, y, 5, 2.5);
    },
    { signal: ac.signal },
  );

  // Animate only while it can be seen and the window is in use.
  const STEP = 1000 / 60;
  const reduced = () => document.documentElement.dataset.motion === 'reduce';
  const wanted = () => !lost && !reduced() && document.visibilityState === 'visible' && document.hasFocus();

  function frame(now: number) {
    raf = requestAnimationFrame(frame);
    // At most ~60–80 draws a second, also on 144/240 Hz screens: it's a background.
    if (now - last < STEP * 0.7) return;
    const dt = Math.min(now - last, 250);
    last = now;
    acc += dt;
    while (acc >= STEP) {
      step();
      acc -= STEP;
    }
    clock += (dt / 1000) * C.flow;
    render();
  }

  function update() {
    const want = wanted();
    if (want && !running) {
      running = true;
      last = performance.now();
      acc = 0;
      raf = requestAnimationFrame(frame);
    } else if (!want && running) {
      running = false;
      cancelAnimationFrame(raf);
    }
  }

  const ro = new ResizeObserver(resize);
  ro.observe(canvas);
  // Theme and motion changes land as attributes / inline variables on <html>.
  const mo = new MutationObserver(() => {
    readColor();
    if (!running && !lost) render();
    update();
  });
  mo.observe(document.documentElement, {
    attributes: true,
    attributeFilter: ['style', 'data-motion', 'data-theme', 'data-scheme'],
  });
  window.addEventListener('focus', update, { signal: ac.signal });
  window.addEventListener('blur', update, { signal: ac.signal });
  document.addEventListener('visibilitychange', update, { signal: ac.signal });
  canvas.addEventListener(
    'webglcontextlost',
    () => {
      lost = true;
      update();
    },
    { signal: ac.signal },
  );

  resize();
  for (let n = 0; n < 6; n++) drop(Math.random() * simW, Math.random() * simH, 4, 1.5);
  update();

  return {
    stop() {
      running = false;
      cancelAnimationFrame(raf);
      ro.disconnect();
      mo.disconnect();
      ac.abort();
    },
  };
}
