/* pando · 01–03, the story: a grove the first chapters scroll over.
 *
 * The stage is sticky; the beats scroll past it. Each beat names a state of
 * the grove (one tree, your repository, branches taking turns, every branch
 * alive). When a beat's words come up to where they rest under the bar
 * (P.storyNow, in site.js, which also shows them), the grove eases towards
 * that beat's state: a jump or a fast scroll goes straight there, and
 * nothing is ever typed, decoded or scrubbed. With reduced motion the grove
 * changes state in one frame; the pause button, on the grove's bottom edge,
 * stops whatever moves by itself.
 */
(function () {
  'use strict';
  const P = window.PANDO;
  const story = document.querySelector('.story');
  if (!story || !P || !P.Grove) return;
  const { clamp, lerp, ss, E } = P.m;
  const C = P.C;
  // 9903 → "9,903", without loading locale data for it
  const commas = n => String(n).replace(/\B(?=(\d{3})+(?!\d))/g, ',');

  const stage = story.querySelector('.stage');
  const canvas = stage.querySelector('canvas');
  const beats = Array.from(story.querySelectorAll('.beat'));
  const seedHud = story.querySelector('.seed-hud');
  const pauseBtn = story.querySelector('.stage-pause');
  const co = {
    err: stage.querySelector('.co.err'), port: stage.querySelector('.co.port'),
    stash: stage.querySelector('.co.stash'), pop: stage.querySelector('.co.pop'), ask: stage.querySelector('.co.ask'),
  };

  // from here on the words show one beat at a time (site.css)
  if (P.storyNow) story.classList.add('story-live');
  const grove = new P.Grove({ ghosts: 20 });
  const view = new P.View(canvas, { maxDpr: 2 });
  view.trackPointer(story);
  const sky = new P.Sky(150, 33);
  const X = grove.X, G = grove.G, BR = P.BR;
  grove.H = Math.max(...grove.trees.map(t => t.H)) + 30; // the tallest crown, in the grove's units
  if (seedHud) seedHud.textContent = `seed 0x5eed · ${commas(P.segments(grove))} segments`;

  /* ---------- what each beat shows ---------- */
  // cam: 0 is the hero's framing (a small grove under the name), 1 the
  // story's (the grove in the lower half, its root in view). lone: the grove
  // cut back to one trunk. bracket: "your repository" under the root.
  const STATES = [
    { cam: 0, lone: 0, labels: 0, green: 0, ghosts: 0.55, glow: 1, root: 1, bracket: 0 },
    { cam: 1, lone: 0, labels: 0, green: 0, ghosts: 0.5, glow: 1.7, root: 1, bracket: 0 },
    { cam: 1, lone: 0, labels: 1, green: 0, ghosts: 0.38, glow: 1.25, root: 1, bracket: 1 },
    { cam: 1, lone: 1, labels: 0, green: 0, ghosts: 0.1, glow: 0.75, root: 0.55, bracket: 0 },
    { cam: 1, lone: 0, labels: 1, green: 1, ghosts: 0.3, glow: 1.9, root: 1, bracket: 0 },
  ];
  const v = Object.assign({}, STATES[0]);
  let target = 0, enteredAt = 0;
  const waves = { gold: -99, green: -99 };
  const clock = () => performance.now() / 1000;
  let paused = false, lp = null;
  const still = () => P.reduced || paused;

  /* ---------- which beat the reader is on ---------- */
  let tops = [], heroBottom = 0;
  const hero = story.querySelector('.hero');
  function measure() {
    const y0 = window.scrollY;
    tops = beats.map(b => b.getBoundingClientRect().top + y0);
    // where the hero's words end, from the top of the page (and the stage)
    heroBottom = hero ? hero.getBoundingClientRect().bottom - story.getBoundingClientRect().top : 0;
  }
  function current() {
    if (P.storyNow) return Math.min(P.storyNow().cur, STATES.length - 1);
    const line = window.scrollY + innerHeight * 0.45;
    let k = 0;
    for (let i = 1; i < tops.length; i++) if (tops[i] <= line) k = i;
    return Math.min(k, STATES.length - 1);
  }
  function setTarget(k) {
    if (k === target) return;
    const from = target;
    target = k; enteredAt = clock();
    if (k === 2 && from < 2) waves.gold = enteredAt;
    if (k === 4 && from < 4) waves.green = enteredAt;
    flips = 0; flipAt = enteredAt;
    if (P.reduced) Object.assign(v, STATES[k]);
    if (lp) lp.kick(P.reduced ? 0 : 2600);
  }

  /* ---------- branches taking turns: the lone trunk's names ---------- */
  const TURNS = [0, 1, 2]; // main → feat/checkout → fix/login-loop
  const FLIP = 1.7;
  let flips = 0, flipAt = 0;
  function lone(now) {
    if (target !== 3 || still()) return { k: flips, F: 1 };
    const n = Math.floor((now - enteredAt - 0.6) / FLIP) + 1;
    if (n > flips) { flips = n; flipAt = enteredAt + 0.6 + (n - 1) * FLIP; }
    return { k: flips, F: flips ? 0.45 + 0.55 * E.out(clamp((now - flipAt) / 0.5)) : 1 };
  }

  /* ---------- the camera ---------- */
  function camera() {
    const w = view.w, h = view.h, portrait = h > w * 1.05;
    const base = Math.max(Math.min(w / 1920, h / 1080), portrait ? w / 1250 : 0);
    // on a short screen the grove keeps lower and smaller, under the words
    const short = h < 720;
    let zH = base * (portrait ? 0.95 : 0.62);
    const gyH = portrait ? (short ? 0.97 : 0.9) : 0.975;
    // under the hero the crowns stay clear of its words: on a short phone
    // the grove is drawn smaller rather than behind them
    const room = gyH * h - heroBottom - 20, tall = grove.H;
    if (heroBottom && tall * zH > room) zH = Math.max(zH * 0.45, room / tall);
    const zS = base * (portrait ? (short ? 0.92 : 1.05) : short ? 0.72 : 0.9), gyS = short ? 0.87 : portrait ? 0.74 : h < 820 ? 0.8 : 0.75;
    const k = E.ioSine(clamp(v.cam));
    const z = lerp(zH, zS, k) * (1 + 0.1 * v.lone);
    const gy = lerp(gyH, gyS, k);
    return { x: 960, y: G - (gy - 0.5) * h / z, z };
  }

  /* ---------- the frame ---------- */
  let t0 = null, last = 0;
  const growDelay = [0.25, 0.05, 0.4, 0.15, 0.5, 0.3];
  const css = (el, k, val) => { const s = el._css || (el._css = {}); val = String(val); if (s[k] !== val) { s[k] = val; el.style[k] = val; } };
  const approach = (a, b, dt, rate) => a + (b - a) * (1 - Math.exp(-rate * dt));

  function frame(nowArg) {
    if (t0 == null) return;
    const now = clock(), dt = last ? Math.min(0.1, now - last) : 0.016;
    last = now;
    const T = STATES[target];
    if (P.reduced) Object.assign(v, T);
    else for (const key in T) v[key] = approach(v[key], T[key], dt, key === 'cam' ? 2.6 : 3.4);

    if (view.resize()) measure();
    const cam = camera(), drift = still() ? 0 : 1;
    view.cam = { x: cam.x + Math.sin(now * 0.31) * 6 * drift, y: cam.y + Math.sin(now * 0.23 + 1) * 3 * drift, z: cam.z };
    view._treePointer = view.pointerScreen ? view.unproj(view.pointerScreen[0], view.pointerScreen[1]) : null;
    const left = view.unproj(0, 0)[0], right = view.unproj(view.w, 0)[0];
    const leaf = still() ? 0 : nowArg;

    view.clear();
    sky.draw(view);
    view.world();
    grove.drawGround(view, 1);

    // how much of each trunk there is
    const grown = i => (P.reduced ? 1 : E.ioSine(clamp((now - t0 - growDelay[i]) / 2.2)));
    const L = lone(now);
    const loneX = lerp(X[0], 960, E.io(clamp(v.lone)));
    const st = X.map((x, i) => {
      if (i === 0) return { x: loneX, F: grown(0) * (v.lone > 0.02 ? lerp(1, L.F, v.lone) : 1) };
      return { x, F: grown(i) * clamp(1 - v.lone * 1.15) };
    });
    const ghostF = (P.reduced ? 1 : E.ioSine(clamp((now - t0 - 0.3) / 2.4))) * clamp(1 - v.lone * 1.1);

    // the one root, and every trunk's way down to it
    const conns = st.map((s, i) => ({ x: s.x, y: G, cdx: grove.trees[i].cdx, pres: clamp(s.F * 3), green: v.green }));
    grove.ghosts.forEach(g => conns.push({ x: g.x, y: g.y, cdx: g.tr.cdx, pres: clamp(ghostF * 3) * 0.5 }));
    grove.drawRoots(view, { alpha: v.root, glow: v.glow, trunks: conns, particles: still() ? 0.6 : 1 }, leaf);
    if (!P.reduced) {
      grove.drawWave(view, (now - waves.gold) / 1.1, '252,228,165', left - 200, right + 300);
      grove.drawWave(view, (now - waves.green) / 1, '124,255,178', left - 200, right + 300);
    }
    grove.drawGhosts(view, ghostF, v.ghosts, leaf);
    st.forEach((s, i) => P.drawTree(view, grove.trees[i], { x: s.x, y: G, s: 1, F: s.F, alpha: 1, green: i === 0 && v.lone > 0.5 ? 0 : v.green, glow: 12 }, leaf));

    // screen space: names, the bracket, the pains
    view.screen();
    labels(st, L);
    bracket();
    callouts(loneX, now);
  }

  function labelSize() { return Math.round(clamp(view.cam.z * 26, 12, 24)); }
  function labels(st, L) {
    const size = labelSize();
    const a = v.labels * clamp(1 - v.lone * 2);
    if (a > 0.01) st.forEach((s, i) => {
      if (s.F < 0.3) return;
      const [sx, sy] = view.proj(s.x, G);
      if (sx < -200 || sx > view.w + 200) return;
      P.drawLabel(view, sx, sy, BR[i], { size, row: i % 2, alpha: a, green: v.green });
    });
    if (v.lone < 0.05) return;
    const [sx, sy] = view.proj(st[0].x, G);
    const name = BR[TURNS[L.k % TURNS.length]];
    const la = clamp(v.lone * 1.5 - 0.3);
    const r = P.drawLabel(view, sx, sy, name, { size: Math.round(size * 1.15), alpha: la });
    if (!r) return;
    // the names it had before, struck through, piling up beneath it
    const ctx = view.ctx, hs = Math.round(size * 0.92), gap = hs * 1.55;
    ctx.save(); ctx.font = `400 ${hs}px ${P.MONO}`; ctx.textAlign = 'center';
    for (let j = 1; j <= Math.min(2, L.k); j++) {
      const was = BR[TURNS[(L.k - j) % TURNS.length]], y = r.ly + gap * j + 4, ww = ctx.measureText(was).width;
      ctx.globalAlpha = la * (0.7 - (j - 1) * 0.22);
      ctx.fillStyle = 'rgba(236,236,236,.85)'; ctx.fillText(was, r.lx, y);
      ctx.strokeStyle = '#e89a3c'; ctx.lineWidth = 2;
      ctx.beginPath(); ctx.moveTo(r.lx - ww / 2 - 4, y - hs * 0.32); ctx.lineTo(r.lx + ww / 2 + 4, y - hs * 0.32); ctx.stroke();
    }
    ctx.restore();
  }

  // "your repository": a bracket under the root
  function bracket() {
    const a = v.bracket;
    if (a <= 0.01) return;
    // on the page's grid (--inset), and above the controls on the bottom edge
    const g = clamp(view.w * 0.05, 16, 72), x0 = Math.max(g, (view.w - 1180) / 2), x1 = view.w - x0;
    const ctx = view.ctx, y = Math.min(view.proj(0, G + 240)[1], view.h - 84);
    const size = Math.round(clamp(view.cam.z * 34, 15, 30)), mid = view.w / 2;
    ctx.save(); ctx.globalAlpha = a;
    ctx.font = `700 ${size}px ${P.MONO}`;
    const s = 'your repository', tw = ctx.measureText(s).width;
    ctx.strokeStyle = 'rgba(242,184,75,.8)'; ctx.lineWidth = 2;
    ctx.beginPath(); ctx.moveTo(x0, y - 14); ctx.lineTo(x0, y); ctx.lineTo(mid - tw / 2 - 20, y); ctx.moveTo(mid + tw / 2 + 20, y); ctx.lineTo(x1, y); ctx.lineTo(x1, y - 14); ctx.stroke();
    ctx.fillStyle = 'rgba(5,5,5,.85)'; ctx.fillRect(mid - tw / 2 - 12, y - size * 0.7, tw + 24, size * 1.3);
    ctx.fillStyle = C.gold; ctx.textAlign = 'center'; ctx.fillText(s, mid, y + size * 0.36);
    ctx.restore();
  }

  // the pains, around the one trunk; each arrives a moment after the last
  const sizes = new Map();
  function callouts(loneX, now) {
    const since = now - enteredAt;
    const at = d => (target === 3 ? (still() ? 1 : ss(d, d + 0.25, since)) : 0) * clamp(v.lone * 1.4 - 0.2);
    const on = (el, a, x, y) => {
      if (!el) return;
      css(el, 'opacity', a.toFixed(3));
      css(el, 'visibility', a <= 0.01 ? 'hidden' : 'visible');
      if (a > 0.01) css(el, 'transform', `translate(${Math.round(x)}px, ${Math.round(y)}px)`);
    };
    if (v.lone < 0.02) { Object.values(co).forEach(el => on(el, 0, 0, 0)); return; }
    const [sx, gy] = view.proj(loneX, G), topY = view.proj(loneX, G - 400)[1];
    const narrow = view.w < 760, W = view.w;
    const sz = el => { let s = sizes.get(el); if (!s) sizes.set(el, (s = [el.offsetWidth, el.offsetHeight])); return s; };
    const w_ = el => sz(el)[0], h_ = el => sz(el)[1];
    const pad = 12;
    // git stash and pop: left of the trunk
    let x = narrow ? pad : sx - w_(co.stash) - Math.max(70, W * 0.07);
    let y = narrow ? lerp(topY, gy, 0.22) : lerp(topY, gy, 0.3);
    on(co.stash, at(0.15), Math.max(pad, x), y);
    on(co.pop, at(1.1), Math.max(pad, x), y + h_(co.stash) * 1.2);
    // the port that is still taken: right of the trunk, at its middle
    x = narrow ? W - w_(co.err) - pad : sx + Math.max(70, W * 0.07);
    y = narrow ? gy + 70 : lerp(topY, gy, 0.55);
    on(co.err, at(0.6), Math.min(x, W - w_(co.err) - pad), y);
    on(co.port, at(0.6), narrow ? sx + 16 : sx + 18, gy - h_(co.port) - 10);
    // the teammate: further up and out
    // on a phone the teammate is in the words already, and there is no room for them
    x = sx - w_(co.ask) - Math.max(90, W * 0.09);
    y = lerp(topY, gy, 0.72);
    if (x < pad) x = sx + Math.max(70, W * 0.07);
    on(co.ask, narrow ? 0 : at(1.7), Math.max(pad, x), y);
  }

  /* ---------- pausing what moves by itself ---------- */
  if (pauseBtn) {
    if (P.reduced) pauseBtn.hidden = true;
    pauseBtn.addEventListener('click', () => {
      paused = !paused;
      pauseBtn.setAttribute('aria-pressed', String(paused));
      document.documentElement.classList.toggle('still', paused);
      if (lp) { lp.pause(paused); lp.kick(0); }
    });
  }

  /* ---------- start ---------- */
  let ticking = false;
  const onScroll = () => {
    if (ticking) return;
    ticking = true;
    requestAnimationFrame(() => { ticking = false; setTarget(current()); });
  };
  window.addEventListener('scroll', onScroll, { passive: true });
  window.addEventListener('resize', () => { sizes.clear(); measure(); onScroll(); });
  window.addEventListener('pando:fonts', () => { sizes.clear(); measure(); });
  new ResizeObserver(() => { measure(); onScroll(); }).observe(story);
  measure();
  const growing = () => t0 == null || clock() - t0 < 3;
  lp = P.loop(stage, frame, { busy: () => !P.reduced && (growing() || clock() - enteredAt < 2.4 || clock() - waves.green < 1.2) });
  P.fontsReady.then(() => {
    t0 = clock();
    measure();
    // a page opened further down starts in that beat's state, not the hero's
    const k = current(); target = -1; setTarget(k);
    if (k > 0) Object.assign(v, STATES[k]);
    lp.kick(P.reduced ? 0 : 3200);
  });
})();
