/* pando · the grove engine
 *
 * The trees, the one root and what moves along it, drawn on a canvas from
 * seeded random numbers: the same seeds grow the same grove on every visit.
 * A grove is built once (geometry); each frame only decides how much of each
 * tree exists, its colour, and where the camera is. Glows are drawn once into
 * sprites and caches, never with a blur in the frame itself. A loop runs only
 * while its canvas is on screen, at 30 frames a second unless something just
 * happened, and not at all with reduced motion. The tail of this file draws the
 * end card's still grove; the story (story.js) and the demo (tui.js) drive their own.
 */
(function () {
  'use strict';
  const P = (window.PANDO = window.PANDO || {});

  const MONO = '"JetBrains Mono", ui-monospace, SFMono-Regular, Menlo, Consolas, monospace';
  const SANS = '-apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", "Segoe UI", Roboto, Arial, sans-serif';
  const C = { gold: '#F2B84B', green: '#7CFFB2', fg: 'rgba(236,236,236,.94)', dim: '#A1A1A6' };
  const reduced = !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);

  /* ---------- math ---------- */
  function mulberry(a) {
    return function () {
      a |= 0; a = (a + 0x6d2b79f5) | 0;
      let t = Math.imul(a ^ (a >>> 15), 1 | a);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }
  function hash(a, b = 0, c = 0) {
    let h = (Math.imul(a | 0, 374761393) + Math.imul(b | 0, 668265263) + Math.imul(c | 0, 1440662683)) | 0;
    h = Math.imul(h ^ (h >>> 13), 1274126177); h ^= h >>> 16;
    return (h >>> 0) / 4294967296;
  }
  const clamp = (v, a = 0, b = 1) => (v < a ? a : v > b ? b : v);
  const lerp = (a, b, k) => a + (b - a) * k;
  const ss = (a, b, t) => { const k = clamp((t - a) / (b - a)); return k * k * (3 - 2 * k); };
  const E = {
    io: k => (k < 0.5 ? 4 * k * k * k : 1 - Math.pow(-2 * k + 2, 3) / 2),
    out: k => 1 - Math.pow(1 - k, 3),
    ioSine: k => -(Math.cos(Math.PI * k) - 1) / 2,
  };

  /* ---------- a tree ---------- */
  // Segments carry [x1, y1, x2, y2, growStart, growEnd, heightFraction], by depth.
  function makeTree(seed, hk, maxD) {
    const r = mulberry(seed), S = [], T = [];
    function grow(x, y, a, len, d, t) {
      const x2 = x + Math.cos(a) * len, y2 = y + Math.sin(a) * len, du = 0.08 + len / 260;
      S.push([x, y, x2, y2, t, t + du, d]);
      if (d >= maxD) { T.push([x2, y2, t + du]); return; }
      const n = d < 2 ? 2 : r() < 0.3 ? 3 : 2;
      for (let k = 0; k < n; k++) {
        let b = a + (k - (n - 1) / 2) * (0.36 + r() * 0.34) + (r() - 0.5) * 0.24;
        b += (-Math.PI / 2 - b) * 0.1; // upward tropism
        grow(x2, y2, b, len * (0.7 + r() * 0.12), d + 1, t + du);
      }
    }
    let x = 0, y = 0, t = 0;
    const nT = 4, tl = (150 + r() * 25) * hk;
    for (let k = 0; k < nT; k++) {
      const len = tl / nT, a = -Math.PI / 2 + (r() - 0.5) * 0.07;
      const x2 = x + Math.cos(a) * len, y2 = y + Math.sin(a) * len, d = len / 240;
      S.push([x, y, x2, y2, t, t + d, 0]);
      if (k >= 2 && maxD > 7) {
        const side = r() < 0.5 ? -1 : 1;
        grow(x2, y2, -Math.PI / 2 + side * (0.95 + r() * 0.3), 21 * hk, 6, t + d);
      }
      x = x2; y = y2; t += d;
    }
    grow(x, y, -Math.PI / 2 + (r() - 0.5) * 0.12, (54 + r() * 8) * hk, 1, t);
    let tm = 0, top = 0;
    for (const s of S) { tm = Math.max(tm, s[5]); top = Math.min(top, s[1], s[3]); }
    const Ht = -top, byD = [];
    for (let d = 0; d <= maxD; d++) byD.push([]);
    for (const s of S) byD[s[6]].push(s[0], s[1], s[2], s[3], s[4] / tm, s[5] / tm, -Math.min(s[1], s[3]) / Ht);
    const tips = [];
    T.forEach((p, i) => tips.push(p[0], p[1], p[2] / tm, hash(seed, i)));
    const w = [], al = [];
    for (let d = 0; d <= maxD; d++) {
      w.push(d === 0 ? 6.2 * hk : Math.max(0.55, 5.2 * Math.pow(0.7, d)));
      al.push(Math.max(0.34, 0.95 - d * 0.065));
    }
    let bx0 = 0, bx1 = 0;
    for (const s of S) { bx0 = Math.min(bx0, s[0], s[2]); bx1 = Math.max(bx1, s[0], s[2]); }
    const bb = [bx0 - 24, -Ht - 24, bx1 + 24, 24];
    return { byD: byD.map(a => new Float32Array(a)), tips: new Float32Array(tips), H: Ht, n: S.length, w, al, bb, spr: {}, cdx: (hash(seed, 5) - 0.5) * 60 };
  }

  /* ---------- a grove: trunks that matter, ghosts behind them, one root ---------- */
  const HK = [1.0, 1.08, 0.95, 1.04, 0.93, 1.02, 0.98, 1.06];
  function Grove(opts) {
    const o = Object.assign({ ground: 680, trunks: [310, 570, 830, 1090, 1350, 1610], ghosts: 12, x0: -300, x1: 2220, seed: 0 }, opts || {});
    const G = (this.G = o.ground);
    this.X = o.trunks.slice();
    this.x0 = o.x0; this.x1 = o.x1;
    this.trees = this.X.map((x, i) => makeTree(101 + i * 37 + o.seed, HK[i % HK.length], 9));
    const gr = mulberry(999 + o.seed), span = (o.x1 - o.x0) / Math.max(1, o.ghosts);
    this.ghosts = [];
    for (let k = 0; k < o.ghosts; k++) {
      const x = o.x0 + (k + 0.5) * span + (gr() - 0.5) * span * 0.4;
      if (this.X.some(m => Math.abs(m - x) < 75)) continue;
      this.ghosts.push({ x, y: G - 10 - gr() * 12, s: 0.62 + gr() * 0.26, tr: makeTree(500 + k * 13 + o.seed, 1, 7), a: 0.7 + gr() * 0.3 });
    }
    // the root: two strands, links between them, rootlets
    const RX0 = (this.RX0 = o.x0 - 400), RX1 = (this.RX1 = o.x1 + 400);
    this.rootY = x => G + 95 + 16 * Math.sin(x * 0.0065 + 1) + 7 * Math.sin(x * 0.021);
    this.deepY = x => G + 185 + 14 * Math.sin(x * 0.0051 + 2.3) + 6 * Math.sin(x * 0.017 + 0.7);
    const main = [], deep = [];
    for (let x = RX0; x <= RX1; x += 8) { main.push([x, this.rootY(x)]); deep.push([x, this.deepY(x)]); }
    const pMain = new Path2D(), pDeep = new Path2D(), pLinks = new Path2D(), pLets = new Path2D();
    main.forEach((p, i) => (i ? pMain.lineTo(p[0], p[1]) : pMain.moveTo(p[0], p[1])));
    deep.forEach((p, i) => (i ? pDeep.lineTo(p[0], p[1]) : pDeep.moveTo(p[0], p[1])));
    const rr = mulberry(21 + o.seed);
    for (let x = RX0 + 150; x < RX1 - 100; x += 125) {
      const xx = x + (rr() - 0.5) * 60, dx = (rr() - 0.5) * 90, y0 = this.rootY(xx), x1 = xx + dx, y1 = this.deepY(x1);
      pLinks.moveTo(xx, y0);
      pLinks.bezierCurveTo(xx + (rr() - 0.5) * 60, lerp(y0, y1, 0.35), x1 + (rr() - 0.5) * 60, lerp(y0, y1, 0.7), x1, y1);
    }
    const rlet = (x, y, a, len, d) => {
      const x2 = x + Math.cos(a) * len, y2 = y + Math.sin(a) * len;
      pLets.moveTo(x, y); pLets.lineTo(x2, y2);
      if (d >= 3) return;
      for (let k = 0; k < 2; k++) rlet(x2, y2, a + (k ? 0.5 : -0.5) + (rr() - 0.5) * 0.35, len * 0.7, d + 1);
    };
    for (let x = RX0 + 100; x < RX1; x += 55) { const xx = x + rr() * 30; rlet(xx, this.rootY(xx), Math.PI / 2 + (rr() - 0.5) * 0.9, 16 + rr() * 10, 0); }
    for (let x = RX0 + 120; x < RX1; x += 66) { const xx = x + rr() * 30; rlet(xx, this.deepY(xx), Math.PI / 2 + (rr() - 0.5) * 0.8, 14 + rr() * 8, 1); }
    this.roots = { main, deep, pMain, pDeep, pLinks, pLets };
    const pr = mulberry(55 + o.seed);
    this.rp = [];
    const nRP = Math.round((RX1 - RX0) / 26);
    for (let k = 0; k < nRP; k++) this.rp.push({ u: pr(), v: (pr() < 0.5 ? -1 : 1) * (0.004 + pr() * 0.01), s: 1.6 + pr() * 2.2, strand: 0 });
    for (let k = 0; k < nRP * 0.5; k++) this.rp.push({ u: pr(), v: (pr() < 0.5 ? -1 : 1) * (0.003 + pr() * 0.008), s: 1.3 + pr() * 1.6, strand: 1 });
    this._cache = {};
  }

  /* ---------- the view: a canvas, a camera, a device pixel ratio ---------- */
  function View(canvas, opts) {
    this.cv = canvas;
    this.ctx = canvas.getContext('2d');
    this.opts = opts || {};
    this.w = 1; this.h = 1; this.dpr = 1;
    this.cam = { x: 960, y: 540, z: 1 };
    this.pointer = null;
    this.resize();
  }
  View.prototype.resize = function () {
    const r = this.cv.getBoundingClientRect();
    const dpr = Math.min(window.devicePixelRatio || 1, this.opts.maxDpr || 2);
    const w = Math.max(1, Math.round(r.width)), h = Math.max(1, Math.round(r.height));
    if (w === this.w && h === this.h && dpr === this.dpr) return false;
    this.w = w; this.h = h; this.dpr = dpr;
    this.cv.width = Math.round(w * dpr); this.cv.height = Math.round(h * dpr);
    return true;
  };
  View.prototype.world = function () {
    const { ctx, dpr, cam, w, h } = this;
    ctx.setTransform(dpr * cam.z, 0, 0, dpr * cam.z, dpr * (w / 2 - cam.x * cam.z), dpr * (h / 2 - cam.y * cam.z));
  };
  View.prototype.screen = function () { this.ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0); };
  View.prototype.proj = function (wx, wy) { const c = this.cam; return [(wx - c.x) * c.z + this.w / 2, (wy - c.y) * c.z + this.h / 2]; };
  View.prototype.unproj = function (sx, sy) { const c = this.cam; return [(sx - this.w / 2) / c.z + c.x, (sy - this.h / 2) / c.z + c.y]; };
  View.prototype.clear = function () {
    this.ctx.setTransform(1, 0, 0, 1, 0, 0);
    this.ctx.clearRect(0, 0, this.cv.width, this.cv.height);
  };
  View.prototype.trackPointer = function (el) {
    const target = el || this.cv;
    const set = e => { const r = this.cv.getBoundingClientRect(); this.pointerScreen = [e.clientX - r.left, e.clientY - r.top]; };
    target.addEventListener('pointermove', set, { passive: true });
    target.addEventListener('pointerdown', set, { passive: true });
    target.addEventListener('pointerleave', () => { this.pointerScreen = null; }, { passive: true });
  };

  /* ---------- drawing ---------- */
  const rgbOf = (g, gold) => [Math.round(lerp(236 + (124 - 236) * g, 246, gold)), Math.round(lerp(236 + (255 - 236) * g, 190, gold)), Math.round(lerp(236 + (178 - 236) * g, 88, gold))];
  // o: { x, y, s, F (growth 0..1), alpha, green (0..1), gold (0..1), shimmer (0..1), glow,
  //      still: leaves drawn into the sprite too, so a grown tree costs one drawImage }
  function drawTree(view, tr, o, now) {
    if (o.F <= 0.0005 || o.alpha <= 0.003) return;
    const ctx = view.ctx, zs = view.cam.z * o.s, F = o.F, g = o.green || 0, gold = o.gold || 0;
    ctx.save(); ctx.translate(o.x, o.y); ctx.scale(o.s, o.s); ctx.lineCap = 'round';
    const [cr, cg, cb] = rgbOf(g, gold);
    // A grown tree in a settled colour is drawn once, glow and all, into a
    // sprite that is reused; the leaves stay live so they can still quake.
    const settled = !view.noSprite && F >= 1 && (g < 0.02 || g > 0.98) && gold < 0.02 && o.alpha > 0.98;
    if (settled) {
      const sc = clamp(Math.round(zs * view.dpr * 4) / 4, 0.25, 2.5);
      const key = (g > 0.5 ? 'g' : 'w') + sc + (o.glow === 0 ? 'n' : '') + (o.still ? 'L' : '');
      let sp = tr.spr[key];
      if (!sp) {
        if (Object.keys(tr.spr).length > 5) tr.spr = {};
        const bb = tr.bb, cv = document.createElement('canvas');
        cv.width = Math.ceil((bb[2] - bb[0]) * sc); cv.height = Math.ceil((bb[3] - bb[1]) * sc);
        const cx = cv.getContext('2d');
        cx.setTransform(sc, 0, 0, sc, -bb[0] * sc, -bb[1] * sc); cx.lineCap = 'round';
        if (g > 0.5 && o.glow !== 0) { cx.shadowColor = 'rgba(124,255,178,.7)'; cx.shadowBlur = (o.glow || 10) * sc; }
        strokeBranches(cx, tr, 1, g, 0, zs, 1);
        if (o.still) { cx.shadowBlur = 0; drawLeaves({ ctx: cx }, tr, { x: 0, y: 0, s: 1, F: 1, alpha: 1, green: g }, 0, zs); }
        sp = tr.spr[key] = cv;
      }
      const bb = tr.bb;
      ctx.drawImage(sp, bb[0], bb[1], bb[2] - bb[0], bb[3] - bb[1]);
      if (o.still) { ctx.restore(); return; }
    } else {
      strokeBranches(ctx, tr, F, g, gold, zs, o.alpha);
    }
    drawLeaves(view, tr, o, now, zs);
    ctx.restore();
  }

  // leaves: the tips, which quake; near the pointer they quake harder and catch the light
  function drawLeaves(view, tr, o, now, zs) {
    const ctx = view.ctx, F = o.F, g = o.green || 0, [cr, cg, cb] = rgbOf(g, o.gold || 0);
    const T = tr.tips, sz = Math.max(2.8, 1.4 / zs);
    const px = view._treePointer && view._treePointer[0] - o.x, py = view._treePointer && view._treePointer[1] - o.y;
    const sh0 = o.shimmer || 0;
    for (let q = 0; q < 3; q++) {
      ctx.beginPath(); let any = false;
      const hot = [];
      for (let i = 0; i < T.length; i += 4) {
        if (T[i + 2] >= F || ((T[i + 3] * 3) | 0) !== q) continue;
        let x = T[i], y = T[i + 1];
        x += Math.sin(now * 2.1 + x * 0.05 + q) * 0.7;
        if (px != null) {
          const dx = x - px / o.s, dy = y - py / o.s, d2 = dx * dx + dy * dy;
          if (d2 < 19600) {
            const k = 1 - Math.sqrt(d2) / 140, ph = now * 26 + T[i + 3] * 40;
            x += Math.sin(ph) * 3.2 * k; y += Math.cos(ph * 0.8) * 1.6 * k;
            if (k > 0.25) { hot.push(x, y); continue; }
          }
        }
        ctx.rect(x - sz / 2, y - sz / 2, sz, sz); any = true;
      }
      const sh = sh0 ? sh0 * (0.5 + 0.5 * Math.sin(now * 23 + q * 2.1)) : 0;
      const a = Math.min(1, (0.5 + 0.45 * g + sh * 0.9) * o.alpha);
      if (any) {
        ctx.fillStyle = sh > 0.05 ? `rgba(${Math.round(lerp(cr, 210, sh))},255,${Math.round(lerp(cb, 225, sh))},${a})` : `rgba(${cr},${cg},${cb},${a})`;
        ctx.fill();
      }
      if (hot.length) {
        ctx.beginPath();
        for (let j = 0; j < hot.length; j += 2) ctx.rect(hot[j] - sz * 0.6, hot[j + 1] - sz * 0.6, sz * 1.2, sz * 1.2);
        ctx.fillStyle = g > 0.5 ? `rgba(200,255,225,${Math.min(1, a + 0.3)})` : `rgba(252,228,165,${Math.min(1, a + 0.35)})`;
        ctx.fill();
      }
    }
  }

  function strokeBranches(ctx, tr, F, g, gold, zs, alpha) {
    const [cr, cg, cb] = rgbOf(g, gold);
    for (let d = 0; d < tr.byD.length; d++) {
      const A = tr.byD[d]; ctx.beginPath(); let any = false;
      for (let i = 0; i < A.length; i += 7) {
        const g0 = A[i + 4]; if (F <= g0) continue;
        const g1 = A[i + 5], p = F >= g1 ? 1 : (F - g0) / (g1 - g0), x1 = A[i], y1 = A[i + 1];
        ctx.moveTo(x1, y1); ctx.lineTo(x1 + (A[i + 2] - x1) * p, y1 + (A[i + 3] - y1) * p); any = true;
      }
      if (!any) continue;
      ctx.lineWidth = Math.max(tr.w[d], 0.9 / zs);
      ctx.strokeStyle = `rgba(${cr},${cg},${cb},${tr.al[d] * alpha})`;
      ctx.stroke();
    }
  }

  // Draw into an offscreen canvas that covers a world rectangle, at a scale that
  // follows the view's zoom in steps; reused while the zoom stays in its step.
  function cached(view, grove, key, rect, paint) {
    const scale = Math.min(1.6, Math.max(0.25, Math.round(view.cam.z * view.dpr * 4) / 4));
    let c = grove._cache[key];
    if (!c || c.scale !== scale) {
      const cv = document.createElement('canvas');
      cv.width = Math.ceil((rect[2] - rect[0]) * scale); cv.height = Math.ceil((rect[3] - rect[1]) * scale);
      const cx = cv.getContext('2d');
      cx.setTransform(scale, 0, 0, scale, -rect[0] * scale, -rect[1] * scale);
      paint(cx, scale);
      c = grove._cache[key] = { cv, scale, rect };
    }
    view.ctx.drawImage(c.cv, c.rect[0], c.rect[1], c.rect[2] - c.rect[0], c.rect[3] - c.rect[1]);
  }

  // the ground line, faint
  Grove.prototype.drawGround = function (view, alpha) {
    const ctx = view.ctx, G = this.G;
    ctx.save();
    ctx.strokeStyle = `rgba(236,236,236,${0.16 * alpha})`; ctx.lineWidth = 1 / view.cam.z;
    ctx.beginPath(); ctx.moveTo(this.RX0, G); ctx.lineTo(this.RX1, G); ctx.stroke();
    ctx.restore();
  };

  /* st: { alpha, glow, trunks: [{x, y, cdx, pres, green}], particles } */
  Grove.prototype.drawRoots = function (view, st, now) {
    const A = st.alpha == null ? 1 : st.alpha; if (A <= 0) return;
    const ctx = view.ctx, gl = st.glow || 1, G = this.G, z = view.cam.z;
    ctx.save();
    ctx.lineCap = 'round';
    ctx.globalAlpha = A;
    cached(view, this, 'roots', [this.RX0, G + 40, this.RX1, G + 300], (cx, sc) => {
      cx.lineCap = 'round';
      cx.strokeStyle = 'rgba(217,130,43,.3)'; cx.lineWidth = Math.max(0.8, 0.9 / sc); cx.stroke(this.roots.pLets);
      cx.strokeStyle = 'rgba(242,184,75,.34)'; cx.lineWidth = Math.max(1, 1.1 / sc); cx.stroke(this.roots.pLinks);
      cx.shadowColor = 'rgba(242,184,75,.9)';
      cx.shadowBlur = 5 * sc; cx.strokeStyle = 'rgba(242,184,75,.5)'; cx.lineWidth = 1.6; cx.stroke(this.roots.pDeep);
      cx.shadowBlur = 11 * sc; cx.strokeStyle = 'rgba(242,184,75,.82)'; cx.lineWidth = 2.5; cx.stroke(this.roots.pMain);
    });
    // extra glow: the cached root again, added on top
    if (gl > 1.02) {
      ctx.globalCompositeOperation = 'lighter';
      ctx.globalAlpha = A * Math.min(1, (gl - 1) * 0.55);
      ctx.drawImage(this._cache.roots.cv, this.RX0, G + 40, this.RX1 - this.RX0, 260);
      ctx.globalCompositeOperation = 'source-over';
    }
    ctx.globalAlpha = 1;
    // connectors: every trunk drinks from the one root; a soft wide stroke under a fine one
    const paths = new Map();
    for (const c of st.trunks || []) {
      if (c.pres <= 0) continue;
      const ex = c.x + c.cdx, ey = this.rootY(ex);
      const k = (c.green > 0.5 ? 0.55 : 0.62) * A * c.pres;
      const col = (c.green > 0.5 ? '124,255,178,' : '242,184,75,') + Math.min(1, k).toFixed(2);
      let p = paths.get(col); if (!p) paths.set(col, (p = new Path2D()));
      p.moveTo(c.x, c.y); p.bezierCurveTo(c.x, c.y + 45, ex, ey - 35, ex, ey);
    }
    paths.forEach((p, col) => {
      const [r, g, b, a] = col.split(',');
      ctx.strokeStyle = `rgba(${r},${g},${b},${(a * 0.22 * gl).toFixed(3)})`; ctx.lineWidth = 6; ctx.stroke(p);
      ctx.strokeStyle = `rgba(${col})`; ctx.lineWidth = 1.7; ctx.stroke(p);
    });
    ctx.restore();
    this.drawParticles(view, st, now);
  };

  // particles riding the root, and climbing each connector into its trunk
  Grove.prototype.drawParticles = function (view, st, now) {
    const ctx = view.ctx, z = view.cam.z, A = st.alpha == null ? 1 : st.alpha;
    const pa = (st.particles == null ? 1 : st.particles) * A;
    if (pa > 0) {
      ctx.fillStyle = `rgba(250,222,150,${0.85 * pa})`; ctx.beginPath();
      const s0 = 1 / Math.max(z, 0.8);
      for (const p of this.rp) {
        const u = (((p.u + p.v * now) % 1) + 1) % 1, q = at(p.strand ? this.roots.deep : this.roots.main, u), s = p.s * s0;
        ctx.rect(q[0] - s / 2, q[1] - s / 2, s, s);
      }
      ctx.fill();
      ctx.beginPath();
      for (const c of st.trunks || []) {
        if (c.pres < 0.5) continue;
        const ex = c.x + c.cdx, ey = this.rootY(ex);
        for (let j = 0; j < 2; j++) {
          const u = 1 - (((now + j * 0.85 + c.x * 0.001) / 1.7) % 1);
          const q = bez(c.x, c.y, c.x, c.y + 45, ex, ey - 35, ex, ey, u), s = 3 * s0;
          ctx.rect(q[0] - s / 2, q[1] - s / 2, s, s);
        }
      }
      ctx.fillStyle = `rgba(255,234,178,${0.9 * pa})`; ctx.fill();
    }
  };

  // the ghosts: the grove behind the named trunks, cached once grown
  Grove.prototype.drawGhosts = function (view, F, alpha, now) {
    if (alpha <= 0) return;
    if (F >= 1) {
      const G = this.G;
      view.ctx.save(); view.ctx.globalAlpha = alpha;
      cached(view, this, 'ghosts', [this.x0 - 160, G - 330, this.x1 + 160, G + 4], cx => {
        const fake = { ctx: cx, cam: { z: 1 }, dpr: 1, noSprite: true };
        for (const g of this.ghosts) drawTree(fake, g.tr, { x: g.x, y: g.y, s: g.s, F: 1, alpha: g.a }, 0);
      });
      view.ctx.restore();
      return;
    }
    for (const g of this.ghosts) drawTree(view, g.tr, { x: g.x, y: g.y, s: g.s, F, alpha: alpha * g.a }, now);
  };

  // a label under a trunk: a leader line from the trunk's foot, the name on a dark plate
  function drawLabel(view, sx, sy, text, o) {
    o = o || {};
    const ctx = view.ctx, size = o.size || 22, row = o.row || 0, a = o.alpha == null ? 1 : o.alpha;
    if (a <= 0 || !text) return;
    const ly = sy + (row ? size * 2.95 : size * 1.6);
    ctx.save();
    ctx.font = `${o.weight || 400} ${size}px ${o.sans ? SANS : MONO}`; const w = ctx.measureText(text).width;
    const lx = clamp(sx, w / 2 + 12, view.w - w / 2 - 12);
    ctx.globalAlpha = a;
    ctx.fillStyle = 'rgba(5,5,5,.8)'; ctx.fillRect(lx - w / 2 - 8, ly - size * 0.95, w + 16, size * 1.3);
    const green = o.green > 0.5;
    ctx.strokeStyle = o.gold ? 'rgba(242,184,75,.7)' : green ? 'rgba(124,255,178,.55)' : 'rgba(236,236,236,.35)'; ctx.lineWidth = 1;
    ctx.beginPath(); ctx.moveTo(sx, sy + 3); ctx.lineTo(sx, ly - size * 0.95); ctx.stroke();
    ctx.fillStyle = o.color || (o.gold ? C.gold : green ? C.green : C.fg); ctx.textAlign = 'center'; ctx.textBaseline = 'alphabetic';
    ctx.fillText(text, lx, ly);
    ctx.restore();
    return { lx, ly, w };
  }

  function at(pts, u) { const f = u * (pts.length - 1), k = Math.floor(f), a = pts[k], b = pts[Math.min(k + 1, pts.length - 1)], e = f - k; return [a[0] + (b[0] - a[0]) * e, a[1] + (b[1] - a[1]) * e]; }
  function bez(x0, y0, x1, y1, x2, y2, x3, y3, u) { const v = 1 - u; return [v * v * v * x0 + 3 * v * v * u * x1 + 3 * v * u * u * x2 + u * u * u * x3, v * v * v * y0 + 3 * v * v * u * y1 + 3 * v * u * u * y2 + u * u * u * y3]; }

  /* ---------- a pocket of its own under a trunk, and a wave along the root ---------- */
  function pocketShape() {
    const qr = P.m.mulberry(91), ring = [];
    for (let k = 0; k <= 64; k++) {
      const a = -Math.PI / 2 + (k / 64) * Math.PI * 2, j = 1 + (qr() - 0.5) * 0.16;
      ring.push([Math.cos(a) * 96 * j, Math.sin(a) * 52 * j]);
    }
    const lets = [];
    for (let k = 0; k < 18; k++) { const a = qr() * Math.PI * 2; lets.push([Math.cos(a) * 96, Math.sin(a) * 52, a, 10 + qr() * 14]); }
    return { ring, lets };
  }
  // a pulse running along the root; u: 0..1 across the visible span [xa, xb]
  Grove.prototype.drawWave = function (view, u, rgb, xa, xb) {
    if (u <= 0 || u >= 1.02) return;
    const ctx = view.ctx, xw = lerp(xa, xb, u);
    ctx.save();
    for (const [fn, off, sc] of [[this.rootY, 0, 1], [this.deepY, 170, 0.7]]) {
      const hx = xw - off - 90, hy = fn(hx), R = 170 * sc;
      const rg = ctx.createRadialGradient(hx, hy, 0, hx, hy, R);
      rg.addColorStop(0, `rgba(${rgb},${0.28 * sc})`); rg.addColorStop(1, `rgba(${rgb},0)`);
      ctx.fillStyle = rg; ctx.fillRect(hx - R, hy - R, R * 2, R * 2);
      for (let j = 0; j < 42; j++) {
        const x = xw - off - j * 13, y = fn(x), a = (1 - j / 42) * sc;
        ctx.fillStyle = `rgba(${rgb},${a.toFixed(3)})`; ctx.beginPath(); ctx.arc(x, y, (5.5 - j * 0.1) * sc, 0, 6.283); ctx.fill();
      }
    }
    ctx.fillStyle = '#FFFFFF'; ctx.beginPath(); ctx.arc(xw, this.rootY(xw), 4, 0, 6.283); ctx.fill();
    ctx.restore();
  };

  // A pocket of its own under a trunk: a line down past the shared root, a ring, a
  // database. o.rgb colours it; o.cylinders draws more than one database.
  Grove.prototype.drawPocket = function (view, x0, k, now, o) {
    if (k <= 0) return;
    o = o || {};
    const ctx = view.ctx, depth = o.depth || 250, sc = o.scale || 1, A = o.alpha == null ? 1 : o.alpha;
    const col = o.rgb || '242,184,75';
    ctx.save(); ctx.translate(x0, this.G); ctx.lineCap = 'round';
    if (!o.noStem) {
      ctx.strokeStyle = `rgba(${col},${0.8 * A})`; ctx.lineWidth = 2;
      const kk = clamp(k * 2.2), len = depth - 52 * sc;
      ctx.beginPath(); ctx.moveTo(0, 0);
      for (let j = 1; j <= 20; j++) { const u = (j / 20) * kk; ctx.lineTo(Math.sin(u * 5) * 6 * u, u * len); }
      ctx.stroke();
    }
    ctx.translate(0, depth); ctx.scale(sc, sc);
    const ps = this.pocketShape || (this.pocketShape = pocketShape()), R = ps.ring, n = Math.floor(R.length * clamp(k * 1.3 - 0.25));
    if (n > 1) {
      ctx.fillStyle = `rgba(${col},${0.07 * A})`;
      ctx.beginPath(); for (let j = 0; j < n; j++) (j ? ctx.lineTo(R[j][0], R[j][1]) : ctx.moveTo(R[j][0], R[j][1]));
      if (n >= R.length) { ctx.closePath(); ctx.fill(); }
      ctx.strokeStyle = `rgba(${col},${0.9 * A})`; ctx.lineWidth = 2.2 / sc; ctx.stroke();
      ctx.strokeStyle = `rgba(${col},${0.35 * A})`; ctx.lineWidth = 1.1 / sc; ctx.beginPath();
      const m = clamp(k * 2 - 1);
      for (const l of ps.lets) { ctx.moveTo(l[0], l[1]); ctx.lineTo(l[0] + Math.cos(l[2]) * l[3] * m, l[1] + Math.sin(l[2]) * l[3] * m); }
      ctx.stroke();
    }
    const c = clamp(k * 1.6 - 0.5);
    if (c > 0) drawCylinder(ctx, 0, 0, c * A, o.cylinders || 1, o.cylColor);
    if (k > 0.6) {
      ctx.fillStyle = `rgba(250,222,150,${(k - 0.6) * 2 * A})`; ctx.beginPath();
      for (let j = 0; j < 12; j++) { const a = now * 0.9 + j * 0.5236; ctx.rect(Math.cos(a) * 80 - 1.8, Math.sin(a) * 40 - 1.8, 3.6, 3.6); }
      ctx.fill();
    }
    ctx.restore();
  };

  function drawCylinder(ctx, cx, cy, a, count, color) {
    ctx.save();
    ctx.globalAlpha = a; ctx.strokeStyle = color || '#FBE3A6'; ctx.lineWidth = 2.2;
    const rw = 30, rh = 9, hh = 38;
    const one = x => {
      ctx.beginPath(); ctx.ellipse(x, cy - hh / 2, rw, rh, 0, 0, 6.283); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(x - rw, cy - hh / 2); ctx.lineTo(x - rw, cy + hh / 2); ctx.ellipse(x, cy + hh / 2, rw, rh, 0, Math.PI, 0, true); ctx.lineTo(x + rw, cy - hh / 2); ctx.stroke();
      ctx.beginPath(); ctx.ellipse(x, cy, rw, rh, 0, Math.PI, 0, true); ctx.stroke();
    };
    if (count === 1) one(cx);
    else for (let j = 0; j < count; j++) one(cx + (j - (count - 1) / 2) * 72);
    ctx.restore();
  }

  /* ---------- a loop that runs only while its canvas is on screen ---------- */
  // At most 30 frames a second, and none while the canvas is off screen or
  // the tab is hidden. Paused, or with reduced motion, it draws one frame and
  // stops; kick(ms) then draws for that long only, so a change still lands.
  function loop(el, frame, o) {
    o = o || {};
    let visible = false, raf = 0, timer = 0, last = 0, until = 0, paused = !!o.still, inTick = false;
    const run = () => { timer = 0; raf = requestAnimationFrame(tick); };
    function tick(ts) {
      raf = 0;
      if (!visible || document.hidden) { last = 0; return; }
      const now = ts / 1000;
      // a kick() from inside the frame only moves `until`: one chain of frames, never two
      inTick = true;
      try { frame(reduced ? 0 : now, last ? Math.min(0.1, now - last) : 0.033); } finally { inTick = false; }
      last = now;
      if ((paused || reduced) && ts >= until) return;
      timer = setTimeout(run, Math.max(0, 33 - (performance.now() - ts)));
    }
    const start = () => { if (!raf && !timer && !inTick && visible && !document.hidden) raf = requestAnimationFrame(tick); };
    // the strip under the chapter bar does not count as on screen: the bar hides it
    new IntersectionObserver(es => { visible = es[es.length - 1].isIntersecting; if (visible) start(); }, { rootMargin: '-80px 0px 0px 0px' }).observe(el);
    document.addEventListener('visibilitychange', start);
    return {
      kick(ms) { until = performance.now() + (ms == null ? 1500 : ms); start(); },
      pause(p) { paused = p; if (!p) start(); },
      redraw: start,
      isVisible: () => visible,
    };
  }

  P.MONO = MONO; P.C = C;
  P.m = { mulberry, hash, clamp, lerp, ss, E };
  P.Grove = Grove; P.View = View;
  P.drawTree = drawTree; P.drawLabel = drawLabel;
  P.loop = loop; P.reduced = reduced;
  // Measure once the mono face is here, or after a moment without it; when it
  // arrives late, 'pando:fonts' tells whoever measured text to measure again.
  const fontLoad = (document.fonts && document.fonts.load) ? document.fonts.load('400 16px "JetBrains Mono"').catch(() => {}) : Promise.resolve();
  fontLoad.then(() => window.dispatchEvent(new Event('pando:fonts')));
  P.fontsReady = Promise.race([fontLoad, new Promise(r => setTimeout(r, 1200))]);

  /* ---------- a sky: a few stars, drawn once per size ---------- */
  function Sky(n, seed) { this.n = n; this.seed = seed; this.cv = null; this.key = ''; }
  Sky.prototype.draw = function (view) {
    const key = view.cv.width + 'x' + view.cv.height;
    if (key !== this.key) {
      this.key = key;
      const cv = (this.cv = document.createElement('canvas')), r = mulberry(this.seed);
      cv.width = view.cv.width; cv.height = view.cv.height;
      const cx = cv.getContext('2d'), d = view.dpr;
      for (let k = 0; k < this.n; k++) {
        const x = r() * cv.width, y = r() * cv.height * 0.8, s = (0.6 + r() * 1.1) * d, a = 0.12 + r() * 0.4;
        cx.fillStyle = r() < 0.12 ? `rgba(242,184,75,${a})` : `rgba(236,236,236,${a})`;
        cx.fillRect(x, y, s, s);
      }
    }
    view.ctx.setTransform(1, 0, 0, 1, 0, 0);
    view.ctx.drawImage(this.cv, 0, 0);
  };

  /* ---------- a still grove: the end card ---------- */
  // Drawn once when it comes near, and again only when its size changes.
  const BR = ['main', 'feat/checkout', 'fix/login-loop', 'pr-128', 'feat/search', 'chore/deps'];
  function still(box) {
    const cv = box && box.querySelector('canvas');
    if (!cv) return;
    let grove = null, view = null, sky = null;
    const paint = () => {
      if (!grove) { grove = new Grove({ seed: 7 }); view = new View(cv, { maxDpr: 2 }); sky = new Sky(90, 7); }
      if (!view.resize() && cv.dataset.drawn) return;
      cv.dataset.drawn = '1';
      grove._cache = {};
      const G = grove.G, w = view.w, h = view.h, narrow = w < 700;
      let z = Math.min(w / 1900, h / 840);
      if (narrow) z = Math.max(z, w / 1100);
      view.cam = { x: narrow ? 830 : 960, y: G - 0.34 * h / z, z };
      view.clear(); sky.draw(view); view.world();
      grove.drawGround(view, 1);
      const conns = grove.X.map((x, i) => ({ x, y: G, cdx: grove.trees[i].cdx, pres: 1, green: 1 }));
      grove.ghosts.forEach(g => conns.push({ x: g.x, y: g.y, cdx: g.tr.cdx, pres: 0.5 }));
      grove.drawRoots(view, { alpha: 1, glow: 1.4, trunks: conns, particles: 0.7 }, 3.1);
      grove.drawGhosts(view, 1, 0.42, 0);
      grove.X.forEach((x, i) => drawTree(view, grove.trees[i], { x, y: G, s: 1, F: 1, alpha: 1, green: 1, glow: 12, still: true }, 0));
      view.screen();
      const size = Math.round(clamp(z * 26, 11, 15));
      grove.X.forEach((x, i) => { const [sx, sy] = view.proj(x, G); if (sx > -80 && sx < w + 80) drawLabel(view, sx, sy, BR[i], { size, row: i % 2, green: 1 }); });
    };
    const io = new IntersectionObserver(es => {
      if (!es.some(e => e.isIntersecting)) return;
      io.disconnect();
      P.fontsReady.then(() => requestAnimationFrame(paint));
      new ResizeObserver(() => requestAnimationFrame(paint)).observe(box);
    }, { rootMargin: '50% 0px' });
    io.observe(box);
  }

  /* ---------- the three data modes: one still drawing each ---------- */
  // Two trunks over one root: the main checkout (left) with the project's
  // database under it, and a worktree (right). The mode decides what the
  // worktree drinks from. Drawn once when it comes near, again on a resize.
  function modeStill(cv, idx) {
    const mode = cv.dataset.mode, MX = 620, WX = 1300, DEPTH = 262;
    let grove = null, view = null;
    const paint = () => {
      if (!grove) { grove = new Grove({ trunks: [MX, WX], ghosts: 7, x0: 150, x1: 1800, seed: 11 + idx }); view = new View(cv, { maxDpr: 2 }); }
      if (!view.resize() && cv.dataset.drawn) return;
      cv.dataset.drawn = '1';
      grove._cache = {};
      const G = grove.G, w = view.w, h = view.h;
      view.cam = { x: 960, y: 640, z: Math.min(w / 1320, h / 870) };
      view.clear(); view.world();
      grove.drawGround(view, 1);
      const conns = [{ x: MX, y: G, cdx: grove.trees[0].cdx, pres: 1 }, { x: WX, y: G, cdx: grove.trees[1].cdx, pres: 1, green: 1 }];
      grove.ghosts.forEach(g => conns.push({ x: g.x, y: g.y, cdx: g.tr.cdx, pres: 0.4 }));
      grove.drawRoots(view, { alpha: 0.85, glow: 1, trunks: conns, particles: 0.6 }, 2.2 + idx);
      grove.drawPocket(view, MX, 1, 1.7, { depth: DEPTH, cylinders: mode === 'namespaced' ? 2 : 1 });
      if (mode === 'isolated') grove.drawPocket(view, WX, 1, 3, { depth: DEPTH, rgb: '124,255,178', cylColor: '#C8FFE0' });
      else {
        const tx = MX + (mode === 'namespaced' ? 36 : 0), ty = G + DEPTH - 22, ctx = view.ctx;
        ctx.save(); ctx.setLineDash([7, 8]);
        ctx.strokeStyle = mode === 'namespaced' ? 'rgba(124,255,178,.8)' : 'rgba(242,184,75,.8)';
        ctx.lineWidth = 2.2 / Math.max(0.5, view.cam.z);
        ctx.beginPath(); ctx.moveTo(WX, G + 8); ctx.bezierCurveTo(WX, G + 170, tx + 200, ty - 60, tx + 34, ty); ctx.stroke();
        ctx.restore();
      }
      grove.drawGhosts(view, 1, 0.22, 0);
      drawTree(view, grove.trees[0], { x: MX, y: G, s: 1, F: 1, alpha: 1, green: 0.85, still: true }, 0);
      drawTree(view, grove.trees[1], { x: WX, y: G, s: 1, F: 1, alpha: 1, green: 1, still: true }, 0);
      view.screen();
      const size = Math.round(clamp(view.cam.z * 30, 11, 15));
      const [mx, my] = view.proj(MX, G), [wx, wy] = view.proj(WX, G);
      drawLabel(view, mx, my, 'main ⌂', { size, green: 1 });
      drawLabel(view, wx, wy, 'feat/login' + (mode === 'isolated' ? ' ▣' : mode === 'namespaced' ? ' ◧' : ''), { size, green: 1 });
      const ctx = view.ctx, ls = Math.round(clamp(view.cam.z * 26, 11, 14));
      ctx.save(); ctx.font = `400 ${ls}px ${MONO}`; ctx.textAlign = 'center';
      const [dx, dy] = view.proj(MX, G + DEPTH + 80);
      ctx.fillStyle = 'rgba(242,184,75,.95)';
      ctx.fillText(mode === 'namespaced' ? 'shop · shop__feat_login' : 'shop · your database', dx, dy);
      if (mode === 'isolated') { const [ix, iy] = view.proj(WX, G + DEPTH + 80); ctx.fillStyle = 'rgba(124,255,178,.95)'; ctx.fillText('its own, on :30010', ix, iy); }
      ctx.restore();
    };
    const io = new IntersectionObserver(es => {
      if (!es.some(e => e.isIntersecting)) return;
      io.disconnect();
      P.fontsReady.then(() => requestAnimationFrame(paint));
      new ResizeObserver(() => requestAnimationFrame(paint)).observe(cv);
    }, { rootMargin: '60% 0px' });
    io.observe(cv);
  }

  P.Sky = Sky;
  P.BR = BR;
  P.segments = g => g.trees.reduce((a, t) => a + t.n, 0) + g.ghosts.reduce((a, x) => a + x.tr.n, 0);
  still(document.querySelector('.endcard'));
  document.querySelectorAll('canvas[data-mode]').forEach(modeStill);
})();
