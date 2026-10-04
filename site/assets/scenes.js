/* pando · the smaller scenes: the three data modes, the first-run setup
 * screen, and the end card. Each runs only while it is on screen. */
(function () {
  'use strict';
  const P = window.PANDO;
  if (!P.Grove) return;
  const { clamp, lerp, ss } = P.m;
  const C = P.C;

  // A scene is built when the reader comes within a screen of it: its grove
  // costs a phone a good part of a second, and many visits never get this far.
  function whenNear(el, build) {
    const io = new IntersectionObserver(es => { if (es.some(e => e.isIntersecting)) { io.disconnect(); build(); } }, { rootMargin: '100% 0px' });
    io.observe(el);
  }

  /* ---------- 06 · the three modes ---------- */
  // Two trunks over one root: the main checkout (left) with the project's
  // database under it, and a worktree (right). The mode decides what the
  // worktree drinks from.
  document.querySelectorAll('canvas[data-mode]').forEach((cv, idx) => whenNear(cv, () => {
    const mode = cv.dataset.mode;
    const grove = new P.Grove({ trunks: [620, 1300], ghosts: 7, x0: 150, x1: 1800, seed: 11 + idx });
    const view = new P.View(cv, { maxDpr: 2 });
    view.trackPointer(cv);
    const G = grove.G, MX = 620, WX = 1300, DEPTH = 262;
    const draw = now => {
      view.resize();
      const f = view.fit(300, 205, 1620, 1050);
      view.cam = { x: f.x, y: f.y, z: f.z };
      view._treePointer = view.pointerScreen ? view.unproj(view.pointerScreen[0], view.pointerScreen[1]) : null;
      view.clear(); view.world();
      grove.drawGround(view, 1);
      const conns = [
        { x: MX, y: G, cdx: grove.trees[0].cdx, pres: 1 },
        { x: WX, y: G, cdx: grove.trees[1].cdx, pres: 1, green: 1 },
      ];
      grove.ghosts.forEach(g => conns.push({ x: g.x, y: g.y, cdx: g.tr.cdx, pres: 0.4 }));
      grove.drawRoots(view, { alpha: 0.85, glow: 1, trunks: conns }, now);
      // the project's own server, under main
      grove.drawPocket(view, MX, 1, now, { depth: DEPTH, cylinders: mode === 'namespaced' ? 2 : 1 });
      if (mode === 'isolated') grove.drawPocket(view, WX, 1, now + 1.3, { depth: DEPTH, rgb: '124,255,178', cylColor: '#C8FFE0' });
      // what the worktree talks to
      if (mode !== 'isolated') {
        const tx = MX + (mode === 'namespaced' ? 36 : 0), ty = G + DEPTH - 22;
        const col = mode === 'namespaced' ? '124,255,178' : '242,184,75';
        const ctx = view.ctx;
        ctx.save(); ctx.setLineDash([7, 8]); ctx.lineDashOffset = -now * 22;
        ctx.strokeStyle = `rgba(${col},.75)`; ctx.lineWidth = 2.2 / Math.max(0.5, view.cam.z);
        ctx.beginPath(); ctx.moveTo(WX, G + 8); ctx.bezierCurveTo(WX, G + 170, tx + 200, ty - 60, tx + 34, ty); ctx.stroke();
        ctx.restore();
      }
      grove.drawGhosts(view, 1, 0.22, now);
      P.drawTree(view, grove.trees[0], { x: MX, y: G, s: 1, F: 1, alpha: 1, green: 0.85 }, now);
      P.drawTree(view, grove.trees[1], { x: WX, y: G, s: 1, F: 1, alpha: 1, green: 1 }, now);
      // labels
      view.screen();
      const size = Math.round(clamp(view.cam.z * 30, 10, 16));
      const [mx, my] = view.proj(MX, G), [wx, wy] = view.proj(WX, G);
      P.drawLabel(view, mx, my, 'main ⌂', { size, green: 1 });
      P.drawLabel(view, wx, wy, 'feat/login' + (mode === 'isolated' ? ' ▣' : mode === 'namespaced' ? ' ◧' : ''), { size, green: 1 });
      const ctx = view.ctx, ls = Math.round(clamp(view.cam.z * 26, 9, 14));
      ctx.save(); ctx.font = `500 ${ls}px ${P.MONO}`; ctx.textAlign = 'center';
      const [dx, dy] = view.proj(MX, G + DEPTH + 78);
      ctx.fillStyle = 'rgba(242,184,75,.9)';
      ctx.fillText(mode === 'namespaced' ? 'shop · shop__feat_login' : 'shop · your database', dx, dy);
      if (mode === 'isolated') { const [ix, iy] = view.proj(WX, G + DEPTH + 78); ctx.fillStyle = 'rgba(124,255,178,.9)'; ctx.fillText('its own, on :30010', ix, iy); }
      ctx.restore();
    };
    // three canvases at once: thirty frames a second is plenty for slow motion
    let lastDraw = -1;
    P.loop(cv, now => { if (!P.reduced && now - lastDraw < 1 / 30) return; if (P.reduced && lastDraw >= 0 && !view.resize()) return; lastDraw = now; draw(now); });
    P.fontsReady.then(() => draw(performance.now() / 1000));
  }));

  /* ---------- 08 · the setup screen: waiting, then green ---------- */
  const setup = document.querySelector('.setup');
  if (setup) {
    const art = setup.querySelector('.setup-art');
    const state = setup.querySelector('.setup-state'), spin = setup.querySelector('.spin');
    const ART = art ? art.textContent.replace(/^\n+|\n+$/g, '').split('\n') : [];
    const SP = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏';
    const STEPS = [
      [0, 'waiting for the setup…', ''],
      [3.2, 'settings saved · v tests them', ''],
      [5.6, 'testing: installing · starting api, web…', ''],
      [8.6, 'You\'re ready to use pando in acme-shop · web answered (HTTP 200)', 'ok'],
    ];
    const CYCLE = 13.5;
    const esc = c => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : c === '>' ? '&gt;' : c);
    const fitArt = () => {
      if (!art) return;
      const w = art.clientWidth - 2, cols = Math.max(...ART.map(l => l.length));
      art.style.fontSize = Math.min(11.5, w / (cols * 0.6)) + 'px';
    };
    let last = -1, lastRow = '', lastTick = -1;
    const frame = now => {
      const e = P.reduced ? STEPS[STEPS.length - 1][0] : now % CYCLE;
      let k = 0; STEPS.forEach((s, i) => { if (e >= s[0]) k = i; });
      const ready = STEPS[k][2] === 'ok';
      if (k !== last) {
        last = k;
        state.textContent = STEPS[k][1];
        setup.classList.toggle('ready', ready);
      }
      spin.textContent = ready ? '✓' : SP[P.reduced ? 0 : Math.floor(now * 10) % 10];
      if (!art) return;
      // the leaves quake: canopy cells trade shades, a few at a time
      const tick = Math.floor(now * 5);
      if (tick === lastTick) return;
      lastTick = tick;
      let html = '';
      ART.forEach((line, y) => {
        let cls = y <= 5 ? 'cn' : y <= 7 ? 'st' : y === 8 ? 'gd' : y <= 10 ? 'rt' : 'lb';
        let s = '';
        for (let x = 0; x < line.length; x++) {
          let c = line[x];
          if (y <= 5 && '░▒▓'.includes(c) && !P.reduced && P.m.hash(x, y, tick) < 0.12) c = '░▒▓'[Math.floor(P.m.hash(x, y + 9, tick) * 3)];
          s += esc(c);
        }
        html += `<span class="${cls}">${s}</span>\n`;
      });
      if (html !== lastRow) { art.innerHTML = html; lastRow = html; }
    };
    whenNear(setup, () => {
      fitArt();
      window.addEventListener('resize', fitArt);
      P.fontsReady.then(fitArt);
      window.addEventListener('pando:fonts', fitArt);
    });
    // A jump to an anchor lays the whole page out first (site.js); the art is
    // sized then too, or it would change height while the page scrolls past.
    if (document.documentElement.classList.contains('laid-out')) fitArt();
    document.addEventListener('click', e => { if (e.target.closest && e.target.closest('a[href^="#"]')) fitArt(); }, true);
    P.loop(setup, now => frame(now));
  }

  /* ---------- the end card: every branch alive ---------- */
  const end = document.querySelector('.endcard canvas');
  if (end) whenNear(end, () => {
    const grove = new P.Grove({ ghosts: 20 });
    const view = new P.View(end, { maxDpr: 2 });
    view.trackPointer(end.parentElement);
    const dust = new P.Dust(120, 71);
    const BR = ['main', 'feat/checkout', 'fix/login-loop', 'pr-128/cart-drawer', 'feat/search', 'chore/deps'];
    const G = grove.G;
    let seen = null;
    new IntersectionObserver(es => es.forEach(e => { if (e.isIntersecting && seen == null) seen = performance.now() / 1000; }), { threshold: 0.35 }).observe(end);
    const frame = now => {
      view.resize();
      const w = view.w, h = view.h, portrait = h > w;
      const z = Math.max(Math.min(w / 1920, h / 1080), portrait ? w / 1300 : 0) * (portrait ? 1 : 0.74);
      view.cam = { x: 960, y: G - ((portrait ? 0.78 : 0.83) - 0.5) * h / z, z };
      view._treePointer = view.pointerScreen ? view.unproj(view.pointerScreen[0], view.pointerScreen[1]) : null;
      view.clear();
      dust.draw(view, now, 0.7);
      view.world();
      const e = seen == null ? 0 : P.reduced ? 9 : now - seen;
      const left = view.unproj(0, 0)[0], right = view.unproj(w, 0)[0];
      const greenAt = x => ss(0.4 + (x - left) / (right - left) * 0.9, 0.4 + (x - left) / (right - left) * 0.9 + 0.35, e);
      grove.drawGround(view, 1);
      const conns = grove.X.map((x, i) => ({ x, y: G, cdx: grove.trees[i].cdx, pres: 1, green: greenAt(x) }));
      grove.ghosts.forEach(g => conns.push({ x: g.x, y: g.y, cdx: g.tr.cdx, pres: 0.5 }));
      grove.drawRoots(view, { alpha: 1, glow: 1 + 1.2 * ss(0.8, 1.8, e), trunks: conns }, now);
      grove.drawWave(view, (e - 0.3) / 1.2, '124,255,178', left - 200, right + 300);
      grove.drawPocket(view, grove.X[2], 1, now, { depth: 250, alpha: 0.9 });
      grove.drawGhosts(view, 1, 0.3, now);
      grove.X.forEach((x, i) => P.drawTree(view, grove.trees[i], { x, y: G, s: i === 3 ? 0.8 : 1, F: 1, alpha: 1, green: greenAt(x) }, now));
      view.screen();
      const size = Math.round(clamp(z * 24, 10, 22));
      grove.X.forEach((x, i) => { const [sx, sy] = view.proj(x, G); if (sx > -100 && sx < w + 100) P.drawLabel(view, sx, sy, BR[i], { size, row: i % 2, green: greenAt(x) }); });
    };
    // full rate while the green runs across it, then the slow motion at half
    const sweeping = () => seen != null && performance.now() / 1000 - seen < 3;
    P.fontsReady.then(() => P.loop(end, now => frame(now), { idle: true, busy: sweeping }));
  });
})();
