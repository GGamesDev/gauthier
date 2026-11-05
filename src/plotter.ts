// Plotter (like Thonny's): numbers printed by the program become curves.
// "temp: 21.5  hum: 40" → series "temp" and "hum". Without a label, series are #1, #2…

const NUMBER = /[-+]?(?:\d+\.?\d*|\.\d+)(?:[eE][-+]?\d+)?/g;
const MAX_POINTS = 400;
const COLORS = ['--accent', '--syn-type', '--ok', '--syn-number', '--warn', '--syn-fn', '--error', '--syn-string'];

interface Series {
  name: string;
  values: number[]; // aligned on the global sample index
  start: number;
}

export class Plotter {
  private series = new Map<string, Series>();
  private order: string[] = [];
  private samples = 0;
  private partial = '';
  private dirty = false;
  private frame = 0;
  visible = false;

  constructor(private canvas: HTMLCanvasElement, private legend: HTMLElement, private empty: HTMLElement) {
    new ResizeObserver(() => this.request()).observe(canvas);
  }

  clear() {
    this.series.clear();
    this.order = [];
    this.samples = 0;
    this.partial = '';
    this.request();
  }

  /** Program output, in any chunk size. */
  feed(text: string) {
    const lines = (this.partial + text).split(/\r?\n/);
    this.partial = lines.pop() ?? '';
    for (const line of lines) this.line(line);
    if (lines.length) this.request();
  }

  private line(line: string) {
    // Strip ANSI colours.
    const clean = line.replace(/\x1b\[[0-9;]*m/g, '');
    const found: [string, number][] = [];
    let last = 0;
    let m: RegExpExecArray | null;
    NUMBER.lastIndex = 0;
    let unnamed = 0;
    while ((m = NUMBER.exec(clean))) {
      const before = clean.slice(last, m.index);
      last = m.index + m[0].length;
      // A digit glued to letters (e.g. "Pico2") is not a measurement.
      if (/[A-Za-z_]$/.test(before)) continue;
      const label = before.match(/([A-Za-z_][\w.]*)\s*[:=]?\s*$/)?.[1];
      unnamed++;
      found.push([label ?? `#${unnamed}`, Number(m[0])]);
    }
    if (!found.length) return;
    const index = this.samples++;
    for (const [name, value] of found) {
      if (!Number.isFinite(value)) continue;
      let s = this.series.get(name);
      if (!s) {
        s = { name, values: [], start: index };
        this.series.set(name, s);
        this.order.push(name);
      }
      // Fill gaps so x stays aligned.
      while (s.start + s.values.length < index) s.values.push(NaN);
      s.values.push(value);
      if (s.values.length > MAX_POINTS) {
        const drop = s.values.length - MAX_POINTS;
        s.values.splice(0, drop);
        s.start += drop;
      }
    }
  }

  request() {
    this.dirty = true;
    if (!this.visible || this.frame) return;
    this.frame = requestAnimationFrame(() => {
      this.frame = 0;
      if (this.dirty) this.draw();
    });
  }

  private color(i: number) {
    return getComputedStyle(document.documentElement).getPropertyValue(COLORS[i % COLORS.length]).trim() || '#888';
  }

  private draw() {
    this.dirty = false;
    const c = this.canvas;
    const dpr = window.devicePixelRatio || 1;
    const w = c.clientWidth;
    const h = c.clientHeight;
    if (!w || !h) return;
    c.width = Math.round(w * dpr);
    c.height = Math.round(h * dpr);
    const g = c.getContext('2d')!;
    g.setTransform(dpr, 0, 0, dpr, 0, 0);
    g.clearRect(0, 0, w, h);

    const has = this.order.length > 0;
    this.empty.hidden = has;
    this.legend.innerHTML = '';
    if (!has) return;

    const end = this.samples;
    const begin = Math.max(0, end - MAX_POINTS);
    let min = Infinity;
    let max = -Infinity;
    for (const s of this.series.values())
      for (const v of s.values) {
        if (Number.isFinite(v)) {
          min = Math.min(min, v);
          max = Math.max(max, v);
        }
      }
    if (min === max) {
      min -= 1;
      max += 1;
    }
    const pad = (max - min) * 0.08;
    min -= pad;
    max += pad;

    const left = 48;
    const right = 10;
    const top = 8;
    const bottom = 18;
    const pw = w - left - right;
    const ph = h - top - bottom;
    const x = (i: number) => left + ((i - begin) / Math.max(1, MAX_POINTS - 1)) * pw;
    const y = (v: number) => top + (1 - (v - min) / (max - min)) * ph;

    // Grid and labels.
    const muted = getComputedStyle(document.documentElement).getPropertyValue('--muted').trim();
    const border = getComputedStyle(document.documentElement).getPropertyValue('--border').trim();
    g.font = '11px ui-monospace, Menlo, monospace';
    g.fillStyle = muted;
    g.strokeStyle = border;
    g.lineWidth = 1;
    for (let k = 0; k <= 4; k++) {
      const v = min + ((max - min) * k) / 4;
      const yy = Math.round(y(v)) + 0.5;
      g.beginPath();
      g.moveTo(left, yy);
      g.lineTo(w - right, yy);
      g.stroke();
      const label = Math.abs(v) >= 1000 || Math.abs(v) < 0.01 ? v.toPrecision(3) : v.toFixed(Math.abs(v) < 10 ? 2 : 1);
      g.fillText(label, 4, yy + 4);
    }

    // Curves.
    this.order.forEach((name, i) => {
      const s = this.series.get(name)!;
      g.strokeStyle = this.color(i);
      g.lineWidth = 1.8;
      g.beginPath();
      let pen = false;
      s.values.forEach((v, k) => {
        const idx = s.start + k;
        if (idx < begin || !Number.isFinite(v)) {
          pen = false;
          return;
        }
        if (pen) g.lineTo(x(idx), y(v));
        else g.moveTo(x(idx), y(v));
        pen = true;
      });
      g.stroke();

      const lastValue = [...s.values].reverse().find(Number.isFinite);
      const item = document.createElement('span');
      item.innerHTML = `<i style="background:${this.color(i)}"></i><b></b> <em></em>`;
      (item.querySelector('b') as HTMLElement).textContent = name;
      (item.querySelector('em') as HTMLElement).textContent = lastValue === undefined ? '' : String(lastValue);
      this.legend.appendChild(item);
    });
  }

  /** For tests: name → last value. */
  snapshot(): Record<string, number> {
    const out: Record<string, number> = {};
    for (const s of this.series.values()) {
      const v = [...s.values].reverse().find(Number.isFinite);
      if (v !== undefined) out[s.name] = v;
    }
    return out;
  }
}
