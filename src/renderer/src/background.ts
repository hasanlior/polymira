/**
 * Animated wave background rendered with WebGL at reduced resolution (cheap on low-end GPUs).
 * Colour follows the focused category/platform hue and eases between changes.
 */
const VERT = `
attribute vec2 p;
void main() { gl_Position = vec4(p, 0.0, 1.0); }
`

const FRAG = `
precision mediump float;
uniform vec2 uRes;
uniform float uTime;
uniform vec3 uTop;
uniform vec3 uMid;
uniform vec3 uGlow;

float hash(vec2 p) { return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453); }

// One soft ribbon: a bright core line plus a wide faint glow.
float ribbon(vec2 uv, float base, float amp, float freq, float speed, float phase, float width) {
  float x = uv.x;
  float y = base
    + amp * sin(x * freq + uTime * speed + phase)
    + amp * 0.45 * sin(x * freq * 2.3 - uTime * speed * 0.6 + phase * 1.9)
    + amp * 0.2 * sin(x * freq * 4.1 + uTime * speed * 1.3);
  float d = abs(uv.y - y);
  return width / (d * d * 900.0 + width) ;
}

void main() {
  vec2 uv = gl_FragCoord.xy / uRes;
  uv.y = 1.0 - uv.y;
  float aspect = uRes.x / uRes.y;
  vec2 q = vec2(uv.x * aspect, uv.y);

  // Base gradient: darker at the top, brightest around the wave line.
  vec3 col = mix(uTop, uMid, smoothstep(0.0, 0.62, uv.y));
  col = mix(col, uTop * 0.6, smoothstep(0.62, 1.0, uv.y));

  // Large soft light sheet behind the ribbons.
  float sheet = exp(-pow((uv.y - 0.6 - 0.04 * sin(q.x * 1.3 + uTime * 0.15)) * 4.5, 2.0));
  col += uGlow * sheet * 0.18;

  float r = 0.0;
  r += ribbon(q, 0.60, 0.050, 1.6, 0.22, 0.0, 0.35);
  r += ribbon(q, 0.62, 0.065, 1.2, 0.17, 2.1, 0.22);
  r += ribbon(q, 0.58, 0.040, 2.1, 0.27, 4.0, 0.18);
  r += ribbon(q, 0.64, 0.080, 0.9, 0.12, 1.3, 0.12);
  r += ribbon(q, 0.57, 0.030, 2.7, 0.31, 5.2, 0.10);
  col += uGlow * r * 0.11;

  // Drifting sparkles along the wave: soft round dots, a few per screen.
  vec2 g = vec2(q.x * 26.0 - uTime * 0.35, q.y * 26.0);
  vec2 cell = floor(g);
  float h = hash(cell);
  vec2 offs = vec2(hash(cell + 7.0), hash(cell + 13.0)) * 0.6 + 0.2;
  float dist = length(fract(g) - offs);
  float dotMask = smoothstep(0.09, 0.0, dist) * step(0.93, h);
  float band = exp(-pow((uv.y - 0.6) * 5.0, 2.0));
  float tw = 0.5 + 0.5 * sin(uTime * 1.7 + h * 40.0);
  col += uGlow * dotMask * band * tw * 0.6;

  // Vignette.
  float v = smoothstep(1.25, 0.35, length((uv - 0.5) * vec2(1.1, 1.3)));
  col *= mix(0.55, 1.0, v);

  gl_FragColor = vec4(col, 1.0);
}
`

type RGB = [number, number, number]

function hsl(h: number, s: number, l: number): RGB {
  const k = (n: number) => (n + h / 30) % 12
  const a = s * Math.min(l, 1 - l)
  const f = (n: number) => l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)))
  return [f(0), f(8), f(4)]
}

function palette(hue: number, night: boolean): { top: RGB; mid: RGB; glow: RGB } {
  if (night) return { top: [0.01, 0.01, 0.02], mid: [0.06, 0.06, 0.08], glow: [0.55, 0.55, 0.6] }
  return { top: hsl(hue, 0.7, 0.06), mid: hsl(hue, 0.65, 0.3), glow: hsl(hue, 0.5, 0.82) }
}

export class WaveBackground {
  private gl: WebGLRenderingContext | null
  private prog: WebGLProgram | null = null
  private u: Record<string, WebGLUniformLocation | null> = {}
  private raf = 0
  private start = performance.now()
  private cur = palette(220, false)
  private target = palette(220, false)
  animate = true

  constructor(private canvas: HTMLCanvasElement) {
    this.gl = canvas.getContext('webgl', { antialias: false, alpha: false, powerPreference: 'low-power' })
    if (!this.gl) return
    const gl = this.gl
    const sh = (type: number, src: string) => {
      const s = gl.createShader(type)!
      gl.shaderSource(s, src)
      gl.compileShader(s)
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) console.error(gl.getShaderInfoLog(s))
      return s
    }
    const p = gl.createProgram()!
    gl.attachShader(p, sh(gl.VERTEX_SHADER, VERT))
    gl.attachShader(p, sh(gl.FRAGMENT_SHADER, FRAG))
    gl.linkProgram(p)
    gl.useProgram(p)
    this.prog = p
    const buf = gl.createBuffer()
    gl.bindBuffer(gl.ARRAY_BUFFER, buf)
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW)
    const loc = gl.getAttribLocation(p, 'p')
    gl.enableVertexAttribArray(loc)
    gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0)
    for (const n of ['uRes', 'uTime', 'uTop', 'uMid', 'uGlow']) this.u[n] = gl.getUniformLocation(p, n)
    window.addEventListener('resize', this.resize)
    this.resize()
    this.loop()
  }

  setHue(hue: number, night = false): void {
    this.target = palette(hue, night)
    if (!this.animate) {
      this.cur = this.target
      this.draw()
    }
  }

  private resize = () => {
    const scale = 0.5
    this.canvas.width = Math.max(1, Math.floor(window.innerWidth * scale))
    this.canvas.height = Math.max(1, Math.floor(window.innerHeight * scale))
    this.gl?.viewport(0, 0, this.canvas.width, this.canvas.height)
    this.draw()
  }

  private ease(a: RGB, b: RGB): RGB {
    return [a[0] + (b[0] - a[0]) * 0.04, a[1] + (b[1] - a[1]) * 0.04, a[2] + (b[2] - a[2]) * 0.04]
  }

  private draw(): void {
    const gl = this.gl
    if (!gl || !this.prog) return
    const t = this.animate ? (performance.now() - this.start) / 1000 : 12
    gl.uniform2f(this.u.uRes, this.canvas.width, this.canvas.height)
    gl.uniform1f(this.u.uTime, t)
    gl.uniform3fv(this.u.uTop, this.cur.top)
    gl.uniform3fv(this.u.uMid, this.cur.mid)
    gl.uniform3fv(this.u.uGlow, this.cur.glow)
    gl.drawArrays(gl.TRIANGLES, 0, 3)
  }

  private loop = () => {
    this.cur = {
      top: this.ease(this.cur.top, this.target.top),
      mid: this.ease(this.cur.mid, this.target.mid),
      glow: this.ease(this.cur.glow, this.target.glow)
    }
    this.draw()
    this.raf = requestAnimationFrame(this.loop)
  }

  destroy(): void {
    cancelAnimationFrame(this.raf)
    window.removeEventListener('resize', this.resize)
  }
}
