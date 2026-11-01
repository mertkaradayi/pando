/* pando · the features, each shown as a small grove.
 *
 * A plate is a figure: a canvas drawn with the grove engine (grove.js), a
 * row of steps under it, and what each step says. The first time a plate
 * comes into view it plays its steps once, a short ease into each, and
 * settles on the last; after that it draws nothing until a step is chosen,
 * or play runs them again. Frames come from P.loop, so none are drawn off
 * screen, never more than 30 a second, and with reduced motion each step is
 * one still frame and nothing plays by itself. Far off screen a plate's
 * canvas lets go of its pixels, and draws them again when it comes back.
 */
(function () {
  'use strict';
  const P = window.PANDO;
  if (!P || !P.Grove || !P.loop) return;
  const { clamp, lerp, ss, E } = P.m;
  const MONO = P.MONO, reduced = P.reduced;
  const TR = 1.35;   // seconds: the ease into a step
  const HOLD = 3.8;  // seconds a step shows while the plate plays
  const clock = () => performance.now() / 1000;

  /* ---------- what the plates draw besides trees ---------- */
  const TONE = {
    gold: ['rgba(242,184,75,.7)', '#F2B84B'],
    green: ['rgba(124,255,178,.55)', '#7CFFB2'],
    white: ['rgba(236,236,236,.3)', 'rgba(236,236,236,.94)'],
    dim: ['rgba(236,236,236,.2)', 'rgba(236,236,236,.66)'],
  };
  // a chip like the grove's names: an opaque tag with a hairline edge. cx, cy: its centre, in screen pixels
  function chip(view, cx, cy, text, o) {
    o = o || {};
    const a = o.alpha == null ? 1 : o.alpha;
    if (a <= 0.01 || !text) return null;
    const ctx = view.ctx, size = o.size || 12, tone = TONE[o.tone || 'white'];
    ctx.save();
    ctx.font = `${o.weight || 400} ${size}px ${MONO}`;
    const tw = ctx.measureText(text).width, pw = Math.round(tw + 16), ph = Math.round(size * 1.5);
    let x = o.align === 'left' ? cx : o.align === 'right' ? cx - pw : cx - pw / 2;
    x = Math.round(clamp(x, 4, view.w - pw - 4)) + 0.5;
    const y = Math.round(clamp(cy - ph / 2, 2, view.h - ph - 2)) + 0.5;
    ctx.globalAlpha = a;
    ctx.beginPath();
    if (ctx.roundRect) ctx.roundRect(x, y, pw, ph, 4); else ctx.rect(x, y, pw, ph);
    ctx.fillStyle = 'rgba(8,7,5,.95)'; ctx.fill();
    ctx.strokeStyle = o.edge || tone[0]; ctx.lineWidth = 1; ctx.stroke();
    ctx.fillStyle = o.color || tone[1]; ctx.textAlign = 'left'; ctx.textBaseline = 'middle';
    ctx.fillText(text, x + 8, y + ph / 2 + 0.5);
    ctx.restore();
    return { x, y, w: pw, h: ph };
  }
  // a chip hung on a trunk: a short leader from the point, to the right
  // unless the right edge is too near; higher up the trunk when it would
  // touch a chip already hung (o.avoid, the boxes hung this frame)
  function tag(view, wx, wy, text, o) {
    o = o || {};
    const a = o.alpha == null ? 1 : o.alpha;
    if (a <= 0.01) return null;
    const ctx = view.ctx, size = o.size || 12, ph = Math.round(size * 1.5);
    ctx.save(); ctx.font = `400 ${size}px ${MONO}`;
    const pw = Math.round(ctx.measureText(text).width + 16); ctx.restore();
    let [sx, sy] = view.proj(wx, wy);
    let side = o.side || 1;
    if (side > 0 && sx + 9 + pw > view.w - 4) side = -1;
    if (side < 0 && sx - 9 - pw < 4) side = 1;
    if (o.avoid) {
      for (let n = 0; n < 4; n++) {
        const x0 = side > 0 ? sx + 9 : sx - 9 - pw;
        if (!o.avoid.some(b => x0 < b.x + b.w + 4 && x0 + pw + 4 > b.x && sy - ph / 2 < b.y + b.h + 3 && sy + ph / 2 + 3 > b.y)) break;
        sy -= ph + 6;
      }
    }
    ctx.save(); ctx.globalAlpha = a;
    ctx.strokeStyle = TONE[o.tone || 'white'][0]; ctx.lineWidth = 1;
    ctx.beginPath(); ctx.moveTo(sx + side * 3, Math.round(sy) + 0.5); ctx.lineTo(sx + side * 9, Math.round(sy) + 0.5); ctx.stroke();
    ctx.restore();
    const box = chip(view, sx + side * 9, sy, text, Object.assign({}, o, { align: side > 0 ? 'left' : 'right' }));
    if (o.avoid && box) o.avoid.push(box);
    return box;
  }
  // a beam of light from a to b, grown to k; screen pixels
  function beam(view, x0, y0, x1, y1, k, rgb, o) {
    if (k <= 0.005) return;
    o = o || {};
    const ctx = view.ctx, a = o.alpha == null ? 1 : o.alpha, x = lerp(x0, x1, k), y = lerp(y0, y1, k);
    ctx.save(); ctx.lineCap = 'round';
    if (o.dash) ctx.setLineDash(o.dash);
    for (const [wd, al] of [[7, 0.1], [3.2, 0.28], [1.3, 0.9]]) {
      ctx.strokeStyle = `rgba(${rgb},${al * a})`; ctx.lineWidth = wd;
      ctx.beginPath(); ctx.moveTo(x0, y0); ctx.lineTo(x, y); ctx.stroke();
    }
    if (k < 1) { ctx.fillStyle = `rgba(255,255,255,${0.9 * a})`; ctx.beginPath(); ctx.arc(x, y, 2.6, 0, 6.283); ctx.fill(); }
    ctx.restore();
  }
  // a browser window, the size of a thumbnail: where a branch answers
  function windowGlyph(view, cx, cy, a, rgb) {
    if (a <= 0.01) return;
    const ctx = view.ctx, w = 46, h = 32, x = Math.round(cx - w / 2) + 0.5, y = Math.round(cy - h / 2) + 0.5;
    ctx.save(); ctx.globalAlpha = a;
    ctx.beginPath(); if (ctx.roundRect) ctx.roundRect(x, y, w, h, 4); else ctx.rect(x, y, w, h);
    ctx.fillStyle = 'rgba(8,7,5,.95)'; ctx.fill();
    ctx.strokeStyle = `rgba(${rgb},.75)`; ctx.lineWidth = 1; ctx.stroke();
    ctx.beginPath(); ctx.moveTo(x, y + 8); ctx.lineTo(x + w, y + 8); ctx.stroke();
    ctx.fillStyle = `rgba(${rgb},.75)`;
    for (let j = 0; j < 3; j++) { ctx.beginPath(); ctx.arc(x + 5 + j * 5, y + 4, 1.3, 0, 6.283); ctx.fill(); }
    ctx.fillStyle = `rgba(${rgb},.5)`;
    ctx.fillRect(x + 6, y + 13, w - 20, 2); ctx.fillRect(x + 6, y + 19, w - 12, 2); ctx.fillRect(x + 6, y + 25, w - 26, 2);
    ctx.restore();
  }
  // a phone, a teammate's: where a shared link lands
  function phoneGlyph(view, cx, cy, a, lit) {
    if (a <= 0.01) return;
    const ctx = view.ctx, w = 24, h = 40, x = Math.round(cx - w / 2) + 0.5, y = Math.round(cy - h / 2) + 0.5;
    ctx.save(); ctx.globalAlpha = a;
    ctx.beginPath(); if (ctx.roundRect) ctx.roundRect(x, y, w, h, 5); else ctx.rect(x, y, w, h);
    ctx.fillStyle = 'rgba(8,7,5,.95)'; ctx.fill();
    ctx.strokeStyle = 'rgba(236,236,236,.6)'; ctx.lineWidth = 1; ctx.stroke();
    ctx.fillStyle = `rgba(124,255,178,${0.12 + 0.5 * lit})`; ctx.fillRect(x + 3, y + 5, w - 6, h - 12);
    ctx.fillStyle = 'rgba(236,236,236,.5)'; ctx.fillRect(x + w / 2 - 3, y + h - 4, 6, 1.5);
    ctx.restore();
  }
  // squares in world units: a checkout's files on disk. style(i) → [fill, stroke] (rgba strings or null)
  function blocks(ctx, x0, y0, cols, rows, sq, gap, style, z) {
    const by = new Map();
    for (let r = 0; r < rows; r++) for (let c = 0; c < cols; c++) {
      const i = r * cols + c, s = style(i);
      if (!s) continue;
      const key = (s[0] || '') + '|' + (s[1] || '');
      let p = by.get(key); if (!p) by.set(key, (p = new Path2D()));
      p.rect(x0 + c * (sq + gap), y0 + r * (sq + gap), sq, sq);
    }
    ctx.save(); ctx.lineWidth = Math.max(1, 1.2 / z);
    by.forEach((p, key) => {
      const [f, s] = key.split('|');
      if (f) { ctx.fillStyle = f; ctx.fill(p); }
      if (s) { ctx.strokeStyle = s; ctx.stroke(p); }
    });
    ctx.restore();
  }
  // a dashed thread between two points in world units, gently bowed
  function thread(ctx, x0, y0, x1, y1, bow, rgb, a, z, off) {
    if (a <= 0.01) return;
    ctx.save(); ctx.setLineDash([5 / z, 6 / z]); ctx.lineDashOffset = off || 0;
    ctx.strokeStyle = `rgba(${rgb},${a})`; ctx.lineWidth = Math.max(1.1, 1.5 / z);
    ctx.beginPath(); ctx.moveTo(x0, y0); ctx.bezierCurveTo(x0, y0 + bow, x1, y1 + bow, x1, y1); ctx.stroke();
    ctx.restore();
  }
  // the camera on a box of the world, inside padding in screen pixels [top, right, bottom, left]
  function frameOn(view, box, pad) {
    const [t, r, b, l] = pad || [0, 0, 0, 0];
    const z = Math.min((view.w - l - r) / (box[2] - box[0]), (view.h - t - b) / (box[3] - box[1]));
    view.cam = { x: (box[0] + box[2]) / 2 - (l - r) / 2 / z, y: (box[1] + box[3]) / 2 - (t - b) / 2 / z, z };
    return z;
  }
  const labelSize = view => Math.round(clamp(view.cam.z * 20, 12, 14));
  // a row for each name under the ground, so neighbours never touch
  function rowsFor(view, xs, names, size) {
    const ctx = view.ctx; ctx.save(); ctx.font = `400 ${size}px ${MONO}`;
    const ws = names.map(n => ctx.measureText(n).width + 18);
    ctx.restore();
    const ends = [-1e9, -1e9];
    return xs.map((x, i) => {
      const l = clamp(x - ws[i] / 2, 4, view.w - ws[i] - 4);
      const row = l >= ends[0] + 8 ? 0 : l >= ends[1] + 8 ? 1 : 0;
      ends[row] = l + ws[i];
      return row;
    });
  }
  function names(view, xs, G, list, size) {
    const pts = xs.map(x => view.proj(x, G));
    const rows = rowsFor(view, pts.map(p => p[0]), list.map(l => l.text), size);
    list.forEach((l, i) => { if (l.alpha !== 0) P.drawLabel(view, pts[i][0], pts[i][1], l.text, { size, row: rows[i], green: l.green || 0, gold: l.gold, alpha: l.alpha == null ? 1 : l.alpha }); });
  }
  const conns = (g, xs, list) => {
    const c = xs.map((x, i) => Object.assign({ x, y: g.G, cdx: g.trees[i].cdx, pres: 1, green: 0 }, list ? list[i] : null));
    g.ghosts.forEach(q => c.push({ x: q.x, y: q.y, cdx: q.tr.cdx, pres: 0.4 }));
    return c;
  };
  const treeTop = (g, i) => g.trees[i].H;
  // the tallest named crown, in world units: the camera keeps it whole
  const crown = g => Math.max(...g.trees.map(t => t.H));

  /* ---------- the plates ---------- */
  // Every grove's root fades out at its ends (fade), so a screen wider than
  // the grove, a phone on its side, sees it trail off rather than stop.
  const SPECS = {};

  // 05 · three commands: a trunk grows, lights, and answers beside the others
  SPECS.run = {
    layout(S, view) {
      const nar = view.w < 600, key = nar ? 'n' : 'w';
      if (S.key !== key) {
        S.key = key;
        S.X = nar ? [660, 960, 1260] : [560, 960, 1360];
        S.g = new P.Grove({ trunks: S.X, ghosts: nar ? 4 : 10, x0: S.X[0] - (nar ? 300 : 560), x1: S.X[2] + (nar ? 300 : 560), seed: 21, fade: 360 });
      }
      const G = S.g.G, sp = nar ? 175 : 330;
      frameOn(view, [S.X[0] - sp, G - crown(S.g) - 8, S.X[2] + sp, G + 205], [52, 0, 4, 0]);
    },
    draw(S, view, st) {
      const g = S.g, G = g.G, X = S.X, t = st.now;
      const F = st.ph === 0 ? E.out(clamp(st.raw * 1.15)) : 1;
      let green = st.v([0, 1, 1]), gold = 0;
      if (st.ph === 1 && st.from === 0) { gold = Math.sin(Math.PI * clamp(st.raw / 0.7)) * 0.8; green = ss(0.5, 1, st.raw); }
      const open = st.v([0, 0, 1]);
      view.clear(); view.world();
      g.drawGround(view, 1);
      g.drawRoots(view, { alpha: 1, glow: 1.1 + 0.3 * green, trunks: conns(g, X, [null, null, { pres: clamp(F * 3), green }]), particles: 0.7 }, t);
      g.drawGhosts(view, 1, 0.24, t);
      X.forEach((x, i) => P.drawTree(view, g.trees[i], { x, y: G, s: 1, F: i === 2 ? F : 1, alpha: 1, green: i === 2 ? green : 1, gold: i === 2 ? gold : 0, glow: 12, still: st.settled }, t));
      view.screen();
      const size = labelSize(view);
      names(view, X, G, [{ text: 'main ⌂', green: 1 }, { text: 'fix/login-loop', green: 1 }, { text: 'feat/checkout', green, alpha: clamp(F * 2) }], size);
      const hung = [];
      [':20177', ':22553', ':22809'].forEach((p, i) => tag(view, X[i], G - 112, p, { tone: 'green', size, alpha: i === 2 ? green : 1, avoid: hung }));
      // every branch you started answers on its own address
      X.forEach((x, i) => {
        const a = open * (i === 2 ? 1 : 0.85), [cx, top] = view.proj(x, G - treeTop(g, i));
        if (a <= 0.01) return;
        const wy = Math.max(20, top - 26);
        beam(view, cx, top + 6, cx, wy + 16, clamp(open * 1.4), '124,255,178', { alpha: 0.6 * a });
        windowGlyph(view, cx, wy, clamp(open * 1.6 - 0.4) * a, '124,255,178');
      });
    },
  };

  // 05 · light on disk: a plain worktree writes every file again; pando's clones
  SPECS.cow = {
    layout(S, view) {
      const nar = view.w < 640, key = nar ? 'n' : 'w';
      if (S.key !== key) {
        S.key = key;
        S.nar = nar;
        S.X = nar ? [600, 1020] : [560, 1260];
        S.g = new P.Grove({ trunks: S.X, ghosts: nar ? 4 : 8, x0: S.X[0] - 380, x1: S.X[1] + 380, seed: 31, fade: 360 });
        // a pad of files under each trunk; below or beside it, node_modules
        S.pad = nar ? { cols: 6, rows: 4, sq: 20, gap: 6 } : { cols: 10, rows: 4, sq: 18, gap: 6 };
      }
      const G = S.g.G, p = S.pad, pw = p.cols * (p.sq + p.gap) - p.gap, ph = p.rows * (p.sq + p.gap) - p.gap;
      S.pw = pw; S.pH = ph;
      const bottom = nar ? G + 236 + ph * 2 + 64 : G + 236 + ph;
      const half = nar ? pw / 2 + 40 : pw + 40;
      frameOn(view, [S.X[0] - half, G - crown(S.g) - 8, S.X[1] + half, bottom], [12, 0, 30, 0]);
    },
    // where a pad sits: k 0 the files, 1 node_modules
    at(S, i, k) {
      const G = S.g.G, x = S.X[i];
      if (S.nar) return [x - S.pw / 2, G + 236 + k * (S.pH + 64)];
      return [k ? x + 14 : x - S.pw - 14, G + 236];
    },
    draw(S, view, st) {
      const g = S.g, G = g.G, X = S.X, t = st.now, z = view.cam.z, p = S.pad, N = p.cols * p.rows;
      const F = st.ph === 0 ? E.out(clamp(st.raw * 1.3)) : 1;
      const cow = st.v([0, 1, 1]), nm = st.v([0, 0, 1]);
      const changed = new Set([Math.floor(N * 0.62) + 1]);
      const fixed = new Set([3, N - 5]);
      view.clear(); view.world();
      g.drawGround(view, 1);
      g.drawRoots(view, { alpha: 1, glow: 1.15, trunks: conns(g, X, [{ green: 0.85 }, { pres: clamp(F * 3), green: 1 }]), particles: 0.65 }, t);
      const ctx = view.ctx;
      // main's files and, with clone, its node_modules
      const [ax, ay] = this.at(S, 0, 0), [bx, by] = this.at(S, 1, 0);
      blocks(ctx, ax, ay, p.cols, p.rows, p.sq, p.gap, () => ['rgba(242,184,75,.62)', null], z);
      // the worktree's files: written one by one, or clones of main's
      const fill = st.ph === 0 ? clamp(st.raw * 1.25) : 1;
      blocks(ctx, bx, by, p.cols, p.rows, p.sq, p.gap, i => {
        if (changed.has(i) && cow > 0.02) return [`rgba(124,255,178,${(0.25 + 0.7 * cow).toFixed(2)})`, null];
        const w = (1 - cow) * (i / N < fill ? 1 : 0);
        if (w > 0.02) return [`rgba(236,236,236,${(0.82 * w).toFixed(2)})`, null];
        return cow > 0.02 ? [null, `rgba(242,184,75,${(0.5 * cow).toFixed(2)})`] : [null, 'rgba(236,236,236,.08)'];
      }, z);
      // the clones share main's blocks: threads from one pad to the other
      const off = -t * 14;
      for (let j = 0; j < 3; j++) {
        const sx = bx + (j + 0.5) * S.pw / 3, tx = ax + (j + 0.5) * S.pw / 3;
        thread(ctx, sx, by - 6, tx, ay - 6, -60 - j * 14, '242,184,75', 0.42 * cow, z, off);
      }
      if (nm > 0.01) {
        const [cx, cy] = this.at(S, 0, 1), [dx, dy] = this.at(S, 1, 1);
        ctx.save(); ctx.globalAlpha = nm;
        blocks(ctx, cx, cy, p.cols, p.rows, p.sq, p.gap, () => ['rgba(242,184,75,.4)', null], z);
        const inst = st.ph === 2 ? ss(0.55, 0.95, st.raw) : 1;
        blocks(ctx, dx, dy, p.cols, p.rows, p.sq, p.gap, i => (fixed.has(i) && inst > 0.02 ? [`rgba(124,255,178,${(0.25 + 0.7 * inst).toFixed(2)})`, null] : [null, 'rgba(242,184,75,.45)']), z);
        ctx.restore();
        // side by side, a thread under the pads; stacked, the names sit there instead
        if (!S.nar) thread(ctx, dx + S.pw * 0.5, dy + S.pH + 6, cx + S.pw * 0.5, cy + S.pH + 6, 70, '242,184,75', 0.42 * nm, z, off);
      }
      g.drawGhosts(view, 1, 0.22, t);
      P.drawTree(view, g.trees[0], { x: X[0], y: G, s: 1, F: 1, alpha: 1, green: 0.85, glow: 12, still: st.settled }, t);
      P.drawTree(view, g.trees[1], { x: X[1], y: G, s: 1, F, alpha: 1, green: 1, glow: 12, still: st.settled }, t);
      view.screen();
      const size = labelSize(view);
      names(view, X, G, [{ text: 'main ⌂', green: 1 }, { text: 'feat/checkout', green: 1, alpha: clamp(F * 2) }], size);
      // what each pad is, under it
      const under = (i, k, text, tone, al) => {
        const [x, y] = this.at(S, i, k), [mx, my] = view.proj(x + S.pw / 2, y + S.pH);
        chip(view, mx, my + 15, text, { tone, size: 12, alpha: al });
      };
      // a narrow screen keeps the names short, so neighbours never touch
      const n = S.nar;
      under(0, 0, 'files', 'gold', 1);
      under(1, 0, cow < 0.5 ? (n ? 'all written' : 'every file written') : '40 written', cow < 0.5 ? 'white' : 'green', clamp(st.ph === 0 && st.from === 0 ? st.raw * 2 : 1));
      under(0, 1, 'node_modules', 'gold', nm);
      under(1, 1, '2 written', 'green', nm);
      // the threads say what the hollow squares are: main's own blocks, shared
      if (cow > 0.01) {
        const mx = (bx + ax) / 2 + S.pw / 2, my = Math.min(by, ay) - 6 - 56;
        const [qx, qy] = view.proj(mx, my);
        chip(view, qx, qy, n ? 'cloned' : 'cloned from main ⌂', { tone: 'gold', size: 12, alpha: cow });
      }
    },
  };

  // 06 · data: where a worktree's data lives, mode by mode
  SPECS.data = {
    layout(S, view) {
      const nar = view.w < 600, key = nar ? 'n' : 'w';
      if (S.key !== key) {
        S.key = key; S.nar = nar;
        S.X = nar ? [640, 1160] : [600, 1300];
        S.g = new P.Grove({ trunks: S.X, ghosts: nar ? 4 : 8, x0: S.X[0] - 420, x1: S.X[1] + 420, seed: 41, fade: 360 });
      }
      const G = S.g.G, sp = nar ? 190 : 330;
      frameOn(view, [S.X[0] - sp, G - crown(S.g) - 8, S.X[1] + sp, G + 262 + 70], [12, 0, 44, 0]);
    },
    draw(S, view, st) {
      const g = S.g, G = g.G, X = S.X, t = st.now, z = view.cam.z, D = 262;
      const shared = st.v([1, 0, 0]), ns = st.v([0, 1, 0]), iso = st.v([0, 0, 1]);
      const enter = st.ph === 0 && st.from === 0 ? ss(0.1, 0.9, st.raw) : 1;
      view.clear(); view.world();
      g.drawGround(view, 1);
      g.drawRoots(view, { alpha: 0.9, glow: 1.1, trunks: conns(g, X, [{ green: 0.85 }, { green: 1 }]), particles: 0.6 }, t);
      const ctx = view.ctx;
      // main's server, under the main checkout: its database, and with namespaced one more beside it
      g.drawPocket(view, X[0], 1, t, { depth: D, cylinders: 0 });
      const cx = X[0], cy = G + D;
      ctx.save(); ctx.translate(cx, cy);
      P.drawCylinder(ctx, -36 * ns, 0, 1, 1, '#FBE3A6');
      if (ns > 0.01) P.drawCylinder(ctx, 36 * ns, 0, ns, 1, '#C8FFE0');
      ctx.restore();
      // what the worktree drinks from: main's database, or its own in main's server
      const lk = Math.max(shared, ns) * enter;
      if (lk > 0.01) {
        const tx = cx + 36 * ns, ty = cy - 22;
        const col = ns > 0.5 ? '124,255,178' : '242,184,75';
        ctx.save(); ctx.setLineDash([7 / z * 0.6, 8 / z * 0.6]); ctx.lineDashOffset = -t * 20;
        ctx.strokeStyle = `rgba(${col},${0.8 * lk})`; ctx.lineWidth = Math.max(1.4, 2 / z * 0.55);
        ctx.beginPath(); ctx.moveTo(X[1], G + 8); ctx.bezierCurveTo(X[1], G + 170, tx + 200, ty - 70, tx + 34, ty); ctx.stroke();
        ctx.restore();
      }
      // isolated: servers of its own
      if (iso > 0.01) g.drawPocket(view, X[1], st.ph === 2 ? clamp(st.raw * 1.1) : iso, t, { depth: D, rgb: '124,255,178', cylColor: '#C8FFE0', alpha: iso });
      g.drawGhosts(view, 1, 0.22, t);
      P.drawTree(view, g.trees[0], { x: X[0], y: G, s: 1, F: 1, alpha: 1, green: 0.85, glow: 12, still: st.settled }, t);
      P.drawTree(view, g.trees[1], { x: X[1], y: G, s: 1, F: 1, alpha: 1, green: 1, glow: 12, still: st.settled }, t);
      view.screen();
      const size = labelSize(view);
      const mark = st.ph === 1 ? ' ◧' : st.ph === 2 ? ' ▣' : '';
      names(view, X, G, [{ text: 'main ⌂', green: 1 }, { text: 'feat/login' + mark, green: 1 }], size);
      // what each database is called
      const [mx, my] = view.proj(cx, cy + 62), dy = size * 1.85;
      const below = my + 12;
      const [sx] = view.proj(cx - 36 * ns, 0), [nx] = view.proj(cx + 36, 0);
      chip(view, sx, below, ns > 0.5 || S.nar ? 'shop' : 'shop · main’s database', { tone: 'gold', size, alpha: 1 });
      chip(view, nx + (S.nar ? 10 : 30), below + dy, 'shop__feat_login', { tone: 'green', size, alpha: ns });
      const [ox, oy] = view.proj(X[1], cy + 62);
      chip(view, ox, oy + 12, S.nar ? 'its own' : 'its own database', { tone: 'green', size, alpha: clamp(iso * 2 - 1) });
      const [rx, ry] = view.proj(cx, cy - 66);
      chip(view, rx, ry, 'postgres :5432', { tone: 'dim', size: 12, alpha: 0.9 });
      if (iso > 0.01) { const [qx, qy] = view.proj(X[1], cy - 66); chip(view, qx, qy, 'postgres :30010', { tone: 'green', size: 12, alpha: clamp(iso * 2 - 1) }); }
    },
  };

  // 07 · several repositories: one branch, a trunk in each, one set
  SPECS.repos = {
    layout(S, view) {
      const nar = view.w < 620, key = nar ? 'n' : 'w';
      if (S.key !== key) {
        S.key = key; S.nar = nar;
        // backend: main, feat/x · frontend: feat/x, main (the branch's trunks face each other)
        S.X = nar ? [430, 650, 910, 1130] : [330, 640, 1000, 1310];
        // two roots, each fading out before the other begins
        const mid = (S.X[1] + S.X[2]) / 2, r = nar ? { inner: 100, pad: 80, fade: 120 } : { inner: 150, pad: 120, fade: 170 };
        S.ga = new P.Grove({ trunks: [S.X[0], S.X[1]], ghosts: nar ? 2 : 4, x0: S.X[0] - (nar ? 110 : 190), x1: mid - r.inner, seed: 51, rootPad: r.pad, fade: r.fade });
        S.gb = new P.Grove({ trunks: [S.X[2], S.X[3]], ghosts: nar ? 2 : 4, x0: mid + r.inner, x1: S.X[3] + (nar ? 110 : 190), seed: 57, rootPad: r.pad, fade: r.fade });
      }
      const G = S.ga.G, sp = S.nar ? 160 : 230;
      frameOn(view, [S.X[0] - sp, G - Math.max(crown(S.ga), crown(S.gb)) - 8, S.X[3] + sp, G + 222], [44, 0, 4, 0]);
    },
    draw(S, view, st) {
      const A = S.ga, B = S.gb, G = A.G, X = S.X, t = st.now;
      const F = st.ph === 0 ? (st.from === 0 ? 0 : 1 - E.io(st.raw)) : st.from === 0 ? E.out(clamp(st.raw * 1.2)) : 1;
      let green = st.v([0, 0, 1]), gold = 0;
      if (st.ph === 2 && st.from !== 2) { gold = Math.sin(Math.PI * clamp(st.raw / 0.7)) * 0.7; green = ss(0.5, 1, st.raw); }
      const set = clamp(st.ph === 1 ? ss(0.45, 1, st.raw) : st.v([0, 1, 1]) * (st.ph === 0 ? 0 : 1)) * (F > 0.3 ? 1 : 0);
      view.clear(); view.world();
      A.drawGround(view, 1); B.drawGround(view, 1);
      A.drawRoots(view, { alpha: 1, glow: 1.1, trunks: conns(A, [X[0], X[1]], [{ green: 0.85 }, { pres: clamp(F * 3), green }]), particles: 0.6 }, t);
      B.drawRoots(view, { alpha: 1, glow: 1.1, trunks: conns(B, [X[2], X[3]], [{ pres: clamp(F * 3), green }, { green: 0.85 }]), particles: 0.6 }, t + 3);
      A.drawGhosts(view, 1, 0.22, t); B.drawGhosts(view, 1, 0.22, t);
      const tr = (g, i, x, f, gr, go) => P.drawTree(view, g.trees[i], { x, y: G, s: 1, F: f, alpha: 1, green: gr, gold: go, glow: 12, still: st.settled }, t);
      tr(A, 0, X[0], 1, 0.85, 0); tr(A, 1, X[1], F, green, gold); tr(B, 0, X[2], F, green, gold); tr(B, 1, X[3], 1, 0.85, 0);
      // under each root, its repository
      const ctx = view.ctx;
      view.screen();
      const size = labelSize(view);
      const repo = (x0, x1, text) => {
        const [a, y] = view.proj(x0, G + 200), [b] = view.proj(x1, G + 200);
        ctx.save(); ctx.strokeStyle = 'rgba(242,184,75,.55)'; ctx.lineWidth = 1;
        ctx.beginPath(); ctx.moveTo(a + 0.5, y - 7); ctx.lineTo(a + 0.5, y + 0.5); ctx.lineTo(b - 0.5, y + 0.5); ctx.lineTo(b - 0.5, y - 7); ctx.stroke(); ctx.restore();
        chip(view, (a + b) / 2, y, text, { tone: 'gold', size });
      };
      repo(A.x0 - 30, A.x1 + 10, 'backend/'); repo(B.x0 - 10, B.x1 + 30, 'frontend/');
      names(view, X, G, [{ text: 'main ⌂', green: 1 }, { text: 'feat/x', green, alpha: clamp(F * 2) }, { text: 'feat/x', green, alpha: clamp(F * 2) }, { text: 'main ⌂', green: 1 }], size);
      // one set: a bracket over both crowns, one row
      const top = Math.max(16, Math.min(view.proj(0, G - treeTop(A, 1))[1], view.proj(0, G - treeTop(B, 0))[1]) - 18);
      const [p1] = view.proj(X[1], 0), [p2] = view.proj(X[2], 0);
      if (set > 0.01) {
        ctx.save(); ctx.globalAlpha = set; ctx.strokeStyle = green > 0.5 ? 'rgba(124,255,178,.7)' : 'rgba(242,184,75,.7)'; ctx.lineWidth = 1.2;
        ctx.beginPath(); ctx.moveTo(p1, top + 10); ctx.lineTo(p1, top); ctx.lineTo(p2, top); ctx.lineTo(p2, top + 10); ctx.stroke(); ctx.restore();
        chip(view, (p1 + p2) / 2, top, green > 0.5 ? 'one port window' : 'feat/x · one set', { tone: green > 0.5 ? 'green' : 'gold', size, alpha: set });
      }
      // one start: the frontend is told the backend's port
      const hung = [];
      tag(view, X[1], G - 150, 'api :24816', { tone: 'green', size, side: -1, alpha: green, avoid: hung });
      tag(view, X[2], G - 150, 'web :24817', { tone: 'green', size, side: 1, alpha: green, avoid: hung });
      if (green > 0.01) {
        const [ax, ay] = view.proj(X[2], G - 100), [bx] = view.proj(X[1], G - 100);
        const k = st.ph === 2 && st.from !== 2 ? ss(0.55, 1, st.raw) : green;
        ctx.save(); ctx.setLineDash([4, 5]); ctx.lineDashOffset = -t * 18;
        ctx.strokeStyle = `rgba(124,255,178,${0.75 * k})`; ctx.lineWidth = 1.3;
        ctx.beginPath(); ctx.moveTo(ax - 4, ay); ctx.lineTo(lerp(ax - 4, bx + 4, k), ay); ctx.stroke(); ctx.restore();
        chip(view, (ax + bx) / 2, ay + 15, '{port:api}', { tone: 'green', size: 12, alpha: k });
      }
      // the frontend had no feat/x: it was made from its own base
      const forked = st.ph === 1 ? ss(0.6, 1, st.raw) : st.ph === 2 ? 1 - green : 0;
      tag(view, X[2], G - 150, 'new, from main', { tone: 'dim', size: 12, side: 1, alpha: forked, avoid: hung.slice(1) });
    },
  };

  // 07 · many at once: marked rows act together
  SPECS.many = {
    layout(S, view) {
      const nar = view.w < 520, key = nar ? 'n' : 'w';
      if (S.key !== key) {
        S.key = key; S.nar = nar;
        S.X = nar ? [560, 760, 960, 1160] : [420, 690, 960, 1230, 1500];
        S.names = nar ? ['main ⌂', 'feat/a', 'feat/b', 'fix/c'] : ['main ⌂', 'feat/a', 'feat/b', 'fix/c', 'feat/d'];
        S.g = new P.Grove({ trunks: S.X, ghosts: nar ? 3 : 6, x0: S.X[0] - 260, x1: S.X[S.X.length - 1] + 260, seed: 61, fade: 360 });
      }
      const G = S.g.G, sp = S.nar ? 120 : 170;
      frameOn(view, [S.X[0] - sp, G - crown(S.g) - 8, S.X[S.X.length - 1] + sp, G + 200], [12, 0, 4, 0]);
    },
    draw(S, view, st) {
      const g = S.g, G = g.G, X = S.X, t = st.now, marked = [1, 2, 3];
      const markA = st.ph === 0 ? clamp(st.raw * 1.6) : st.ph === 1 ? 1 : 1 - clamp(st.raw * 2);
      const per = X.map((x, i) => {
        if (!marked.includes(i)) return { F: 1, green: i === 0 ? 1 : 0, gold: 0 };
        const j = marked.indexOf(i);
        if (st.ph === 1) {
          const done = 0.45 + j * 0.17; // each reported as it finishes
          const e = st.from === 1 ? 1 : st.raw;
          return { F: st.from === 2 ? E.out(clamp(st.raw * 1.3)) : 1, green: ss(done, done + 0.12, e), gold: e < done ? 0.35 + 0.35 * Math.sin(t * 7 + j) : 0 };
        }
        if (st.ph === 2) { const s0 = 0.08 + j * 0.12; return { F: st.from === 2 ? 0 : 1 - E.io(clamp((st.raw - s0) / 0.55)), green: 1, gold: 0 }; }
        return { F: st.from === 2 ? E.out(clamp(st.raw * 1.3)) : 1, green: st.from === 1 ? 1 - st.k : 0, gold: 0 };
      });
      view.clear(); view.world();
      g.drawGround(view, 1);
      g.drawRoots(view, { alpha: 1, glow: 1.1, trunks: conns(g, X, per.map(p => ({ pres: clamp(p.F * 3), green: p.green }))), particles: 0.65 }, t);
      g.drawGhosts(view, 1, 0.22, t);
      X.forEach((x, i) => P.drawTree(view, g.trees[i], { x, y: G, s: 1, F: per[i].F, alpha: 1, green: per[i].green, gold: per[i].gold, glow: 12, still: st.settled }, t));
      view.screen();
      const size = labelSize(view);
      names(view, X, G, S.names.map((n, i) => {
        const m = marked.includes(i) && markA > 0.5;
        return { text: (m ? '+ ' : '') + n, green: per[i].green, gold: m, alpha: clamp(per[i].F * 2) };
      }), size);
    },
  };

  // 07 · share: a public link, beamed up from a running trunk
  SPECS.share = {
    layout(S, view) {
      const nar = view.w < 520, key = nar ? 'n' : 'w';
      if (S.key !== key) {
        S.key = key; S.nar = nar;
        S.X = [700, 1080];
        S.g = new P.Grove({ trunks: S.X, ghosts: nar ? 3 : 5, x0: 400, x1: 1400, seed: 71, fade: 360 });
      }
      const G = S.g.G;
      frameOn(view, [S.nar ? 520 : 470, G - crown(S.g) - 8, S.nar ? 1300 : 1360, G + 195], [64, 0, 4, 0]);
    },
    draw(S, view, st) {
      const g = S.g, G = g.G, X = S.X, t = st.now;
      const on = st.v([1, 0]), up = st.ph === 0 ? clamp(st.raw * 1.4) : on;
      view.clear(); view.world();
      g.drawGround(view, 1);
      g.drawRoots(view, { alpha: 1, glow: 1.1, trunks: conns(g, X, [{ green: 1 }, { green: 1 }]), particles: 0.65 }, t);
      g.drawGhosts(view, 1, 0.22, t);
      X.forEach((x, i) => P.drawTree(view, g.trees[i], { x, y: G, s: 1, F: 1, alpha: 1, green: 1, gold: 0, glow: 12, still: st.settled }, t));
      view.screen();
      const size = labelSize(view);
      names(view, X, G, [{ text: 'main ⌂', green: 1 }, { text: 'feat/checkout', green: 1 }], size);
      tag(view, X[1], G - 112, ':22809', { tone: 'green', size, side: -1 });
      const [cx, top] = view.proj(X[1], G - treeTop(g, 1));
      const ly = Math.max(18, top - 46);
      beam(view, cx, top + 4, cx, ly + 12, up, '242,184,75', { alpha: on });
      const box = chip(view, cx, ly, 'quiet-aspen-grove.trycloudflare.com', { tone: 'gold', size: 12, alpha: clamp(up * 2 - 1) * on });
      // a teammate's phone, wherever the link reaches
      if (box) {
        // beside the link where there is room; a phone-wide plate has none
        const right = box.x + box.w + 36 < view.w, left = box.x - 36 > 0;
        if (right || left) phoneGlyph(view, right ? box.x + box.w + 20 : box.x - 20, ly + 6, clamp(up * 2 - 1) * on, clamp(up * 2 - 1.2));
      }
      if (st.ph === 1) chip(view, cx, ly, 'unshared · the link is gone', { tone: 'dim', size: 12, alpha: clamp(st.raw * 2 - 0.6) });
    },
  };

  // 07 · pull requests: a teammate's, planted as a trunk you can start
  SPECS.prs = {
    layout(S, view) {
      const nar = view.w < 520, key = nar ? 'n' : 'w';
      if (S.key !== key) {
        S.key = key; S.nar = nar;
        S.X = [660, 1080];
        S.g = new P.Grove({ trunks: S.X, ghosts: nar ? 3 : 5, x0: 380, x1: 1380, seed: 81, fade: 360 });
      }
      const G = S.g.G;
      frameOn(view, [S.nar ? 500 : 440, G - crown(S.g) - 8, S.nar ? 1290 : 1360, G + 195], [12, 0, 4, 0]);
    },
    draw(S, view, st) {
      const g = S.g, G = g.G, X = S.X, t = st.now;
      const list = st.ph === 0 ? clamp(st.raw * 1.6) : st.ph === 1 ? 1 - ss(0, 0.5, st.from === 1 ? 1 : st.raw) : 0;
      const F = st.ph === 1 ? (st.from === 1 ? 1 : E.out(clamp((st.raw - 0.25) * 1.4))) : st.ph === 2 ? (st.from === 0 ? E.out(clamp(st.raw * 1.3)) : 1) : (st.from === 0 ? 0 : 1 - st.k);
      let green = st.v([0, 0, 1]), gold = 0;
      if (st.ph === 2 && st.from !== 2) { gold = Math.sin(Math.PI * clamp(st.raw / 0.7)) * 0.7; green = ss(0.5, 1, st.raw); }
      view.clear(); view.world();
      g.drawGround(view, 1);
      g.drawRoots(view, { alpha: 1, glow: 1.1, trunks: conns(g, X, [{ green: 1 }, { pres: clamp(F * 3), green }]), particles: 0.65 }, t);
      g.drawGhosts(view, 1, 0.22, t);
      P.drawTree(view, g.trees[0], { x: X[0], y: G, s: 1, F: 1, alpha: 1, green: 1, glow: 12, still: st.settled }, t);
      P.drawTree(view, g.trees[1], { x: X[1], y: G, s: 0.82, F, alpha: 1, green, gold, glow: 12, still: st.settled }, t);
      view.screen();
      const size = labelSize(view);
      names(view, X, G, [{ text: 'main ⌂', green: 1 }, { text: 'pr-128/cart-drawer', green, alpha: clamp(F * 2) }], size);
      tag(view, X[1], G - 100, '◍ #128', { tone: 'green', size, side: -1, alpha: clamp(F * 2 - 0.6) });
      tag(view, X[1], G - 160, ':23417', { tone: 'green', size, side: -1, alpha: green });
      // the open pull requests, as the picker lists them; ⏎ plants one
      if (list > 0.01 || (st.ph === 1 && st.raw < 0.6)) {
        const [cx] = view.proj(X[1], 0), y0 = 22, dy = size * 1.9;
        const rows = ['#131 fix VAT rounding', '#128 cart drawer', '#124 search filters'];
        rows.forEach((r, i) => {
          if (i === 1 && st.ph === 1) {
            const e = st.from === 1 ? 1 : clamp(st.raw / 0.5), [tx, ty] = view.proj(X[1], G - 200);
            chip(view, lerp(cx, tx, E.io(e)), lerp(y0 + dy, ty, E.io(e)), r, { tone: 'gold', size, alpha: 1 - ss(0.7, 1, e) });
            return;
          }
          chip(view, cx, y0 + i * dy, r, { tone: i === 1 ? 'gold' : 'dim', size, alpha: list });
        });
      }
    },
  };

  // 08 · the first run: your agent writes its answers beside the grove, never
  // into it; then pando check grows a throwaway trunk, tests it, and removes it
  SPECS.check = {
    layout(S, view) {
      const nar = view.w < 560, key = nar ? 'n' : 'w';
      if (S.key !== key) {
        S.key = key; S.nar = nar;
        S.X = [700, 1120];
        S.g = new P.Grove({ trunks: S.X, ghosts: nar ? 4 : 7, x0: 360, x1: 1460, seed: 91, fade: 360 });
      }
      const G = S.g.G;
      frameOn(view, [S.nar ? 520 : 430, G - crown(S.g) - 8, S.nar ? 1330 : 1390, G + 200], [40, 0, 4, 0]);
    },
    draw(S, view, st) {
      const g = S.g, G = g.G, X = S.X, t = st.now, p = st.ph - 1, ctx = view.ctx;
      let F = 1, green = 0, gold = 0;
      if (st.ph === 0) F = st.from > 0 && st.from < 4 ? 1 - E.io(clamp(st.raw * 1.4)) : 0;
      if (p === 0 || ((p === 1 || p === 2) && (st.from === 0 || st.from === 4))) F = E.out(clamp(st.raw * 1.2));
      if (p === 1) gold = st.settled ? 0.45 : 0.3 + 0.3 * Math.sin(t * 6);
      if (p === 2) { green = st.from === 3 ? 1 : ss(0.4, 0.9, st.raw); gold = (1 - green) * 0.45; }
      if (p === 3) { green = 1; F = st.from === 4 ? 0 : 1 - E.io(clamp(st.raw * 1.2)); }
      const ready = p === 3 ? ss(0.5, 1, st.from === 4 ? 1 : st.raw) : 0;
      const agent = st.v([1, 0, 0, 0, 0]);
      view.clear(); view.world();
      g.drawGround(view, 1);
      g.drawRoots(view, { alpha: 1, glow: 1 + 0.7 * ready, trunks: conns(g, X, [{ green: 0.85 * ready }, { pres: clamp(F * 3), green }]), particles: 0.65 }, t);
      if (p === 3 && !st.settled) { const xa = view.unproj(0, 0)[0], xb = view.unproj(view.w, 0)[0]; g.drawWave(view, ss(0.35, 1, st.raw), '124,255,178', xa - 200, xb + 200); }
      g.drawGhosts(view, 1, 0.22, t);
      P.drawTree(view, g.trees[0], { x: X[0], y: G, s: 1, F: 1, alpha: 1, green: 0.85 * ready, glow: 12, still: st.settled }, t);
      P.drawTree(view, g.trees[1], { x: X[1], y: G, s: 0.9, F, alpha: 0.85, green, gold, glow: 12, still: st.settled }, t);
      view.screen();
      const size = labelSize(view);
      names(view, X, G, [{ text: 'main ⌂', green: ready }, { text: '(no branch)', green, gold: gold > 0.2, alpha: clamp(F * 2) }], size);
      // your agent answers through pando, into ~/.pando: beside the grove, not in it
      if (agent > 0.01) {
        const a = chip(view, 8, 20, 'your coding agent', { tone: 'white', size: 12, align: 'left', alpha: agent });
        const b = chip(view, view.w - 8, 20, '~/.pando', { tone: 'gold', size: 12, align: 'right', alpha: agent });
        if (a && b) {
          const k = st.ph === 0 && st.from === 0 ? ss(0.15, 0.85, st.raw) : 1, x0 = a.x + a.w + 4, x1 = b.x - 4, y = 20;
          ctx.save(); ctx.globalAlpha = agent; ctx.setLineDash([4, 5]); ctx.lineDashOffset = -t * 18;
          ctx.strokeStyle = 'rgba(242,184,75,.8)'; ctx.lineWidth = 1.3;
          ctx.beginPath(); ctx.moveTo(x0, y); ctx.lineTo(lerp(x0, x1, k), y); ctx.stroke(); ctx.restore();
        }
        tag(view, X[0], G - 120, 'untouched', { tone: 'white', size: 12, side: -1, alpha: agent * (st.ph === 0 && st.from === 0 ? ss(0.6, 1, st.raw) : 1) });
      }
      const ta = clamp(F * 2 - 0.6);
      tag(view, X[1], G - 70, 'a1b2c3d', { tone: 'dim', size: 12, side: -1, alpha: p === 0 || p === 3 ? ta : p > 0 ? 1 : 0 });
      const svc = p === 1 ? clamp(st.raw * 1.5) : p === 2 ? 1 : p === 3 ? ta : 0;
      tag(view, X[1], G - 130, 'api :29848', { tone: green > 0.5 ? 'green' : 'gold', size, side: 1, alpha: svc });
      tag(view, X[1], G - 180, 'web :29849', { tone: green > 0.5 ? 'green' : 'gold', size, side: 1, alpha: svc });
      // asked for its first page
      const [cx, top] = view.proj(X[1], G - treeTop(g, 1) * 0.9);
      const ok = p === 2 ? clamp(st.raw * 1.4) : p === 3 ? ta : 0;
      beam(view, cx, top + 4, cx, Math.max(16, top - 26) + 10, ok, '124,255,178', { alpha: ok });
      chip(view, cx, Math.max(16, top - 26), 'GET / → HTTP 200', { tone: 'green', size: 12, alpha: clamp(ok * 2 - 1) });
      const [mx, my] = view.proj(X[0], G - treeTop(g, 0));
      chip(view, mx, Math.max(16, my - 22), '✓ acme-shop is ready', { tone: 'green', size, alpha: ready });
    },
  };

  /* ---------- a plate: steps, a loop, and plays once ---------- */
  function plate(fig, spec) {
    const art = fig.querySelector('.plate-art'), cv = art && art.querySelector('canvas');
    if (!cv) return;
    const btns = Array.from(fig.querySelectorAll('.ps'));
    const says = Array.from(fig.querySelectorAll('.say'));
    const lines = Array.from(fig.querySelectorAll('[data-ph]'));
    const play = fig.querySelector('.plate-play');
    const meter = fig.querySelector('.plate-meter');
    const N = btns.length || says.length;
    const last = N - 1, still = fig.dataset.still != null ? +fig.dataset.still : last;
    // before it first plays, a plate shows where its first step begins
    const S = { ph: reduced ? still : 0, from: reduced ? still : 0, t0: reduced ? -99 : Infinity, view: null, built: false };
    let auto = false, played = false, timer = 0, due = 0, left = 0, seen = false, lp = null;

    function show() {
      btns.forEach((b, i) => b.setAttribute('aria-pressed', String(i === S.ph)));
      says.forEach((s, i) => s.classList.toggle('on', i === S.ph));
      lines.forEach(l => l.classList.toggle('off', +l.dataset.ph > S.ph));
      if (meter) {
        const s = says[S.ph];
        if (s && s.dataset.mw) {
          meter.style.setProperty('--w', s.dataset.mw);
          meter.querySelector('.pm-l').textContent = s.dataset.ml || '';
          meter.querySelector('.pm-v').textContent = s.dataset.mv || '';
          meter.classList.toggle('us', s.dataset.mus != null);
        }
      }
      fig.classList.toggle('auto', auto);
      if (play) {
        play.querySelector('.lbl').textContent = auto ? 'pause' : 'play';
        play.setAttribute('aria-label', auto ? 'Pause the steps' : 'Play the steps again');
        play.classList.toggle('on', auto);
      }
    }
    function go(i) {
      // the step that was showing is where the ease starts from
      S.from = clock() - S.t0 < TR * 0.5 ? S.from : S.ph;
      S.ph = i; S.t0 = clock();
      show();
      if (lp) lp.kick(reduced ? 0 : TR * 1000 + 200);
      if (auto) schedule(HOLD * 1000);
    }
    function schedule(ms) { clearTimeout(timer); due = performance.now() + ms; timer = setTimeout(advance, ms); }
    function advance() { timer = 0; if (S.ph < last) go(S.ph + 1); else stop(); }
    function stop() { auto = false; clearTimeout(timer); timer = 0; fig.classList.remove('hold'); show(); }
    function start() { auto = true; played = true; go(0); }
    btns.forEach((b, i) => b.addEventListener('click', () => { if (auto) stop(); fig.querySelector('.plate-says').setAttribute('aria-live', 'polite'); go(i); }));
    if (play) {
      if (reduced) play.hidden = true;
      else { play.hidden = false; play.addEventListener('click', () => { if (auto) stop(); else start(); }); }
    }

    function frame() {
      if (!S.built) { S.view = new P.View(cv, { maxDpr: 2 }); S.built = true; S.dirty = true; }
      const view = S.view;
      if (view.resize() || S.dirty) { S.dirty = false; spec.layout(S, view); [S.g, S.ga, S.gb].forEach(g => { if (g) g._cache = {}; }); }
      const raw = reduced ? 1 : clamp((clock() - S.t0) / TR), k = E.io(raw); // Infinity: 0
      const st = { ph: S.ph, from: S.from, raw, k, now: reduced ? 2.2 : clock(), settled: raw >= 1, v: a => lerp(a[S.from], a[S.ph], k) };
      spec.draw(S, view, st);
    }
    lp = P.loop(art, frame, { still: true });
    show();

    // the first time most of its art is in view, it plays; off screen it waits
    new IntersectionObserver(es => {
      const e = es[es.length - 1];
      if (e.intersectionRatio >= 0.45 && !seen) {
        seen = true;
        if (!reduced && !played) start(); else lp.kick(0);
      }
      if (!auto) return;
      if (!e.isIntersecting && timer) { left = Math.max(400, due - performance.now()); clearTimeout(timer); timer = 0; fig.classList.add('hold'); }
      else if (e.intersectionRatio >= 0.45 && !timer) { fig.classList.remove('hold'); schedule(left || HOLD * 1000); }
    }, { threshold: [0, 0.45] }).observe(art);
    // far away, the canvas gives its pixels back
    new IntersectionObserver(es => {
      if (es[es.length - 1].isIntersecting || !S.view) return;
      cv.width = 1; cv.height = 1; S.view.w = 0; S.view.h = 0; S.dirty = true;
      [S.g, S.ga, S.gb].forEach(g => { if (g) { g._cache = {}; g.trees.forEach(tr => { tr.spr = {}; }); } });
    }, { rootMargin: '300% 0px' }).observe(fig);
    new ResizeObserver(() => { S.dirty = true; lp.kick(0); }).observe(art);
    window.addEventListener('pando:fonts', () => lp.kick(0));
  }

  P.fontsReady.then(() => {
    document.querySelectorAll('.plate[data-plate]').forEach(f => { const s = SPECS[f.dataset.plate]; if (s) plate(f, s); });
  });
})();
