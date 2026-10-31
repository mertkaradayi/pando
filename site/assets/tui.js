/* pando · the demo: a replica of the TUI you can press keys in.
 *
 * A character grid drawn like the real one (header, worktree table, detail
 * pane, footer; dialogs over them), driven by the real keymap, over a
 * pretend repository. Below it, the same state drawn as a grove: a trunk per
 * worktree, green while it runs, a pocket of its own when it is isolated.
 * Ports are the ones pando would really give this project (see ports.js).
 */
(function () {
  'use strict';
  const P = window.PANDO;
  const demo = document.getElementById('demo');
  if (!demo || !P.Grove) return;
  const { clamp, lerp, ss, E } = P.m;
  /* ---------- what only the demo draws: a pocket of its own, a wave, a spark ---------- */
  const Grove = P.Grove;
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

  // a spark that flies from one point to another along an arc, leaving a trail
  function comet(view, x0, y0, x1, y1, k, rgb, size, lift) {
    if (k <= 0 || k > 1.25) return;
    const ctx = view.ctx; size = size || 6; lift = lift == null ? 150 : lift;
    ctx.save();
    const pt = u => { const mx = (x0 + x1) / 2, my = Math.min(y0, y1) - lift; const v = 1 - u; return [v * v * x0 + 2 * v * u * mx + u * u * x1, v * v * y0 + 2 * v * u * my + u * u * y1]; };
    const kk = Math.min(1, k);
    for (let j = 0; j < 14; j++) {
      const u = kk - j * 0.025; if (u < 0) break;
      const [x, y] = pt(E.io(u)), a = 1 - j / 14;
      ctx.fillStyle = j === 0 ? `rgba(255,255,255,${a})` : `rgba(${rgb},${a})`;
      ctx.beginPath(); ctx.arc(x, y, size * (1 - j / 18), 0, 6.283); ctx.fill();
    }
    if (k >= 1) { const e = (k - 1) / 0.25; ctx.strokeStyle = `rgba(${rgb},${1 - e})`; ctx.lineWidth = 2.5; ctx.beginPath(); ctx.arc(x1, y1, 8 + e * 50, 0, 6.283); ctx.stroke(); }
    ctx.restore();
  }

  const scr = demo.querySelector('.tui');
  const cv = demo.querySelector('.demo-grove canvas');
  const chat = demo.querySelector('.chat');
  const live = document.getElementById('demo-live');
  const keysEl = demo.querySelector('.keys');

  const ROOT = '/Users/you/code/my-app', PID = P.projectId(ROOT);
  const PUBLIC = 'https://quiet-aspen-grove.trycloudflare.com';
  const SPIN = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏';
  const now = () => performance.now() / 1000;
  const h32 = (s, k = 0) => P.m.hash(Array.from(s).reduce((a, c) => (a * 31 + c.charCodeAt(0)) | 0, 7), k);

  /* ---------- the pretend project ---------- */
  function mkWt(branch, o) {
    const wt = Object.assign({ branch, main: false, st: 'stopped', mode: 'shared', last: 'shared', ports: null, since: 0, pr: null, prTitle: '', git: 'clean', gitLong: 'clean · even with main', public: false, born: -99, gone: 0, logs: [], nextLog: 0, proc: 0, slot: 0, changed: -99, busy: null }, o || {});
    wt.dir = wt.main ? 'my-app' : P.dirName(branch);
    return wt;
  }
  function assignPorts(wt) {
    if (wt.ports) return;
    const base = P.deriveBase(PID, wt.dir);
    wt.ports = { api: base, web: base + 1 };
  }
  let S;
  function reset() {
    const t = now();
    S = {
      wts: [
        mkWt('main', { main: true, slot: 0, git: '', gitLong: 'clean · the main checkout' }),
        mkWt('feat/checkout', { slot: 1, st: 'running', since: t - 131, git: '↑2 ✎', gitLong: '✎ uncommitted changes · ↑2 ahead of main' }),
        mkWt('feat/search', { slot: 2, pr: '◍ #124', prTitle: 'Search: filters and facets', git: '↓6', gitLong: 'clean · ↓6 behind main' }),
      ],
      sel: 1, ov: null, msg: null, msgs: [], armed: null, logsSeen: false, sharedOnce: false, prOnce: false,
      chat: null, comet: null, cap: null, dirty: true, t0: t,
    };
    S.wts.forEach(assignPorts);
    S.wts[1].logs = seedLogs(S.wts[1]);
    S.wts[1].born = t - 999; S.wts[2].born = t - 999; S.wts[0].born = t - 999;
    setCap('This is pando.', 'every worktree, what runs and where · press a key');
  }

  /* ---------- logs ---------- */
  const LINES = {
    api: ['GET /api/cart 200 in 14ms', 'GET /api/cart/items 200 in 22ms', 'POST /api/checkout/summary 201 in 41ms', 'warn: slow query on orders (212ms)',
      'GET /api/health 200 in 1ms', '{"level":"info","msg":"order summary built","items":3,"ms":9}', 'GET /api/cart 200 in 11ms', 'error: coupon service returned 503, retrying', 'POST /api/session 200 in 18ms'],
    web: ['GET /checkout 200 in 38ms', '○ Compiling /checkout ...', '✓ Compiled /checkout in 1.8s', 'GET / 200 in 12ms', 'warn: Fast Refresh had to perform a full reload', 'GET /login 200 in 9ms'],
  };
  const lvl = s => (/\berror\b|✗|failed/i.test(s) ? 'E' : /\bwarn/i.test(s) ? 'W' : '');
  function seedLogs(wt) {
    const L = [];
    L.push({ src: 'install', text: 'pnpm install --frozen-lockfile' }, { src: 'install', text: 'Lockfile is up to date, resolution step is skipped' }, { src: 'install', text: 'Done in 2.3s' });
    L.push({ src: 'api', text: `listening on ${wt.ports.api}` }, { src: 'web', text: `VITE_API_URL=http://localhost:${wt.ports.api}` }, { src: 'web', text: `listening on ${wt.ports.web}` });
    for (let k = 0; k < 9; k++) { const src = k % 3 === 1 ? 'web' : 'api'; L.push({ src, text: LINES[src][(k * 5) % LINES[src].length] }); }
    return L.map(l => Object.assign(l, { lvl: lvl(l.text) }));
  }
  function addLog(wt, src, text) { wt.logs.push({ src, text, lvl: lvl(text), t: now() }); if (wt.logs.length > 400) wt.logs.splice(0, 100); if (S.ov && S.ov.type === 'logs' && cur() === wt) wt.flashLog = now(); }

  /* ---------- helpers ---------- */
  const vis = () => S.wts.filter(w => !w.gone);
  const cur = () => vis()[S.sel] || vis()[0];
  const url = wt => `http://localhost:${wt.ports.web}`;
  const age = s => (s < 60 ? `${Math.floor(s)}s` : s < 3600 ? `${Math.floor(s / 60)}m` : `${Math.floor(s / 3600)}h`);
  const glyph = wt => (wt.busy || wt.st === 'starting' ? ['◌', 'g'] : wt.st === 'running' ? ['●', 'gr'] : wt.st === 'failed' ? ['✗', 'rd'] : ['○', 'd']);
  function tell(text) { if (live) live.textContent = text; }
  function say(text, icon = '›', cls = 'g') { S.msg = { text, icon, cls, t: now() }; S.msgs.unshift(text); S.msgs.length = Math.min(S.msgs.length, 50); if (live && AP.user) live.textContent = text; S.dirty = true; }
  function setCap(text, cmd) { S.cap = { text, cmd, t: now() }; }

  /* ---------- actions (the same verbs the CLI has) ---------- */
  function start(wt, mode) {
    if (wt.main) mode = 'shared';
    assignPorts(wt);
    const was = wt.st === 'running';
    wt.st = 'starting'; wt.mode = mode; wt.last = mode; wt.since = now(); wt.readyAt = now() + (mode === 'isolated' ? 2.4 : 1.6); wt.changed = now();
    if (!was) wt.logs.push({ src: 'api', text: '', lvl: '', gap: true });
    say(`started ${wt.branch}, waiting for it to be ready — ${url(wt)}`);
    const flag = mode === 'shared' ? '' : ` --${mode}`;
    if (wt.main) { say('the main checkout: pando runs its processes and nothing else — no install, no hooks'); setCap('The main checkout runs too.', `pando start main`); }
    else if (mode === 'isolated') setCap('Its own database, if you want one.', `pando start ${wt.branch} --isolated`);
    else if (mode === 'namespaced') setCap('Its own database, in your server.', `pando start ${wt.branch} --namespaced   (experimental)`);
    else setCap('On ports of its own.', `pando start ${wt.branch}${flag}  →  api ${wt.ports.api} · web ${wt.ports.web}`);
    if (wt.main) note('main', wt); else { note('started', wt); mission('started'); }
  }
  function stop(wt) {
    wt.st = 'stopped'; wt.public = false; wt.changed = now();
    say(`stopped ${wt.branch}`, '✓', 'gr');
    setCap('Stopped. Its ports stay its own.', `pando stop ${wt.branch}`);
    note('stopped', wt);
    if (S.chat && S.chat.wt === wt) S.chat = null;
  }
  function create(branch, o) {
    const exists = S.wts.find(w => w.branch === branch && !w.gone);
    if (exists) { S.sel = vis().indexOf(exists); say(`${branch} already has a worktree — selected it`); return; }
    const used = new Set(S.wts.map(w => w.slot));
    let slot = 1; while (used.has(slot)) slot++;
    if (slot >= SLOTX.length) { say('this replica has room for five worktrees — d removes one', '✗', 'rd'); return; }
    const wt = mkWt(branch, Object.assign({ slot, born: now(), busy: { what: 'creating', stage: 'installing', t: now(), until: now() + 1.4 } }, o || {}));
    wt.logs = [{ src: 'install', text: 'pnpm install --frozen-lockfile', lvl: '' }];
    S.wts.splice(1, 0, wt); // newest first, after the main checkout
    S.sel = vis().indexOf(wt);
    if (!o || !o.pr) { setCap('A new branch, in a checkout of its own.', `pando new ${branch}`); note('created', wt); mission('created'); }
    return wt;
  }
  function remove(wt) {
    wt.gone = now(); wt.st = 'stopped'; wt.public = false;
    say(`removed ${wt.branch} — the branch is kept`, '✓', 'gr');
    setCap('Removed. The branch is kept.', `pando rm ${wt.branch}`);
    note('removed', wt);
    S.sel = Math.max(0, Math.min(S.sel, vis().length - 1));
  }
  function share(wt) {
    wt.public = true; wt.changed = now(); S.sharedOnce = true;
    say(`${wt.branch} is public at ${PUBLIC} — O opens it, C copies it`, '✓', 'gr');
    setCap('No staging. No deploy.', `pando share ${wt.branch}`);
    S.comet = { wt, t: now() }; S.chat = { wt, t: now() };
    note('shared', wt); mission('shared');
  }

  /* ---------- keys ---------- */
  const PRS = [
    { n: 131, title: 'fix VAT rounding', who: '@sam', branch: 'fix/vat-rounding' },
    { n: 128, title: 'cart drawer', who: '@alex', branch: 'cart-drawer', fork: true },
    { n: 124, title: 'Search: filters and facets', who: '@kim', branch: 'feat/search' },
  ];
  function key(k) {
    const t = now();
    S.dirty = true;
    const wt = cur();
    const ov = S.ov;
    // a pending double press
    if (S.armed && S.armed.key !== k) { S.armed = null; if (k === 'esc') { say('cancelled'); return; } }
    if (ov) return overlayKey(ov, k, wt);
    if (S.leader) {
      const at = S.leader; S.leader = 0;
      if (t - at < 3 && k === 'g') return say('space g: the git menu — fetch, pull, rebase or merge, the commands shown first — in the real one');
      if (t - at < 3 && k === 'p') return say(wt.pr ? `space p opens ${wt.pr.replace('◍ ', '')} on GitHub — in the real one` : `${wt.branch} has no pull request — p lists the open ones`);
    }
    switch (k) {
      case ' ': S.leader = t; return say('space g opens the git menu — in the real one');
      case '+': return say('+ marks rows, and s x r d then act on every marked one — in the real one');
      case ',': return say(', sorts the list: by PR, newest first, last run first, by name — in the real one');
      case 'j': case '↓': S.sel = Math.min(vis().length - 1, S.sel + 1); break;
      case 'k': case '↑': S.sel = Math.max(0, S.sel - 1); break;
      case 'g': S.sel = 0; break;
      case 'G': S.sel = vis().length - 1; break;
      case '⏎':
        if (wt.busy) return say(`already busy ${wt.busy.what} ${wt.branch}`);
        if (wt.main) { if (wt.st === 'stopped') start(wt, 'shared'); else say(`${wt.branch} is the main checkout, on the project's own services — r restarts it`); return; }
        S.ov = { type: 'mode', sel: ['shared', 'namespaced', 'isolated'].indexOf(wt.st === 'stopped' ? wt.last : wt.mode) };
        tell(`Choose a mode for ${wt.branch}: shared, namespaced or isolated. Up and down move, enter starts it`);
        return;
      case 's': if (wt.busy) return; if (wt.st !== 'stopped') return say(`${wt.branch} is already running — ${url(wt)}`); start(wt, wt.last); return;
      case 'i': case 'S': {
        if (wt.busy) return;
        const m = k === 'i' ? 'isolated' : 'shared';
        if (wt.main && k === 'i') return say(`${wt.branch} is the main checkout, and it runs on the project's own services`, '✗', 'rd');
        if (wt.st === 'stopped') return start(wt, m);
        if (wt.mode === m) { if (S.armed && S.armed.key === k) { S.armed = null; start(wt, m); } else { S.armed = { key: k, t }; say(`restart ${wt.branch} ${m}? ${k} again to confirm · esc cancels`); } return; }
        S.ov = { type: 'switch', to: m }; return;
      }
      case 'x':
        if (wt.st === 'stopped') return say(`${wt.branch} is not running — s starts it`);
        if (S.armed && S.armed.key === 'x') { S.armed = null; stop(wt); } else { S.armed = { key: 'x', t }; say(`stop ${wt.branch}? x again to confirm · esc cancels`); }
        return;
      case 'X': { const up = vis().filter(w => w.st !== 'stopped'); if (!up.length) return say('nothing is running'); S.ov = { type: 'stopall', up }; return; }
      case 'r': case 'P':
        if (wt.busy) return say(`already busy ${wt.busy.what} ${wt.branch}`);
        if (wt.st === 'stopped') { if (k === 'r') return start(wt, wt.last); return say(`${wt.branch} is not running — s starts it`); }
        if (S.armed && S.armed.key === k) {
          S.armed = null;
          const pn = ['api', 'web'][wt.proc];
          const wasPublic = k === 'r' && wt.public;
          if (wasPublic) { wt.public = false; if (S.chat && S.chat.wt === wt) S.chat = null; }
          start(wt, wt.mode);
          if (wasPublic) say(`${wt.branch}: a whole restart closes its public URL — t shares it again`);
          setCap(k === 'r' ? 'Restarted, on the same ports.' : 'One process restarted; the other kept running.', k === 'r' ? `pando restart ${wt.branch}` : `pando restart ${wt.branch} --only ${pn}`);
        }
        else { S.armed = { key: k, t }; say(k === 'r' ? `restart ${wt.branch}? r again to confirm · esc cancels` : `restart ${['api', 'web'][wt.proc]} of ${wt.branch}? P again to confirm · esc cancels`); }
        return;
      case 'l': S.ov = { type: 'logs', tab: 0, filter: 0 }; S.logsSeen = true; setCap('Its own logs.', `pando logs ${wt.branch} -f`); note('logs', wt); mission('logs'); return;
      case 't':
        if (wt.st !== 'running') return say(`${wt.branch} is not running — s starts it`);
        S.ov = { type: wt.public ? 'unshare' : 'share' }; return;
      case 'o': if (wt.st !== 'running') return say(`${wt.branch} is not running — s starts it`); say(`opened ${url(wt)} (in your terminal this opens your browser)`); setCap('Its URL, in your browser.', `pando open ${wt.branch}`); return;
      case 'O': case 'C': if (!wt.public) return say(`${wt.branch} is not shared — t shares it`); if (k === 'C') copy(PUBLIC, `copied ${PUBLIC}`); else say(`opened ${PUBLIC} (in your terminal this opens your browser)`); return;
      case 'c': if (wt.st !== 'running') return say(`${wt.branch} is not running — s starts it`); copy(url(wt), `copied ${url(wt)}`); return;
      case 'y': { const p = wt.main ? ROOT.replace('/Users/you', '~') : `~/.pando/projects/${PID}/worktrees/${wt.dir}`; copy(p, `copied ${p}`); setCap('Where it lives.', `cd "$(pando path ${wt.branch})"`); return; }
      case 'n': S.ov = { type: 'new', text: '', sel: 0 }; tell('New worktree: type a branch, enter makes it'); return;
      case 'p': S.ov = { type: 'prs', q: '', sel: 0 }; tell('Open pull requests: type to filter, enter makes a worktree for one, escape cancels'); return;
      case 'd': if (wt.busy) return say(`already busy ${wt.busy.what} ${wt.branch}`); if (wt.main) return say(`${wt.branch} is the main checkout — pando runs it, but never removes it`, '✗', 'rd'); S.ov = { type: 'remove' }; return;
      case 'tab': wt.proc = (wt.proc + 1) % 2; return;
      case '?': S.ov = { type: 'help', scroll: 0 }; return;
      case 'm': S.ov = { type: 'msgs' }; return;
      case '!': return say(`a shell in ${wt.branch} — in your terminal (a tmux window inside tmux)`);
      case 'e': return say(`opens ${wt.branch} in $VISUAL / $EDITOR — in your terminal`);
      case '/': return say('/ filters by branch or name — try it in the real one');
      case 'T': return say('T picks a colour theme: pando, Catppuccin, Flexoki, GitHub, Gruvbox, Kanagawa, Tokyo Night…');
      case 'R': return say('refreshed');
      case 'v': return say('v tests the setup with `pando check`: a throwaway worktree, installed, started, removed');
      case 'a': copy('Set up pando here: run `pando init --agent` and follow what it says.', 'copied the setup prompt, for your coding agent'); return;
      case 'q': case 'esc': return say('q quits the real one; this replica stays');
      default: S.dirty = false;
    }
  }
  function overlayKey(ov, k, wt) {
    const close = () => { S.ov = null; };
    if (ov.type === 'help' || ov.type === 'msgs') {
      if (k === 'j' || k === '↓') ov.scroll = (ov.scroll || 0) + 1;
      else if (k === 'k' || k === '↑') ov.scroll = Math.max(0, (ov.scroll || 0) - 1);
      else if (k === 'esc' || k === 'q' || k === '?' || k === 'm') close();
      return;
    }
    if (ov.type === 'logs') {
      if (k === 'esc' || k === 'q') return close();
      if (/^[1-4]$/.test(k)) ov.tab = +k - 1;
      else if (k === 'tab') ov.tab = (ov.tab + 1) % 4;
      else if (k === 'f') ov.filter = (ov.filter + 1) % 3;
      else if (k === 'e' || k === 'E') ov.jump = now();
      return;
    }
    if (ov.type === 'mode') {
      if (k === '↑' || k === 'k') ov.sel = Math.max(0, ov.sel - 1);
      else if (k === '↓' || k === 'j') ov.sel = Math.min(2, ov.sel + 1);
      else if (k === 'esc' || k === 'q') close();
      else if (k === '⏎') {
        const m = ['shared', 'namespaced', 'isolated'][ov.sel];
        close();
        if (wt.st !== 'stopped' && m === wt.mode) return say(`${wt.branch} already runs ${m} — r restarts it`);
        start(wt, m);
      }
      return;
    }
    if (ov.type === 'new' || ov.type === 'prs') {
      const field = ov.type === 'new' ? 'text' : 'q';
      if (k === 'esc') return close();
      if (k === 'backspace') { ov[field] = ov[field].slice(0, -1); ov.sel = 0; return; }
      if (k === '↑') { ov.sel = Math.max(0, ov.sel - 1); return; }
      if (k === '↓') { ov.sel = ov.sel + 1; return; }
      if (k === '⏎') {
        if (ov.type === 'new') {
          const rows = newRows(ov), r = rows[Math.min(ov.sel, rows.length - 1)];
          close();
          if (!r) return;
          if (r.has) { const j = vis().findIndex(w => w.branch === r.branch); if (j >= 0) S.sel = j; return say(`${r.branch} already has a worktree — selected it`); }
          const w = create(r.branch);
          if (w) say(`creating ${r.branch} · checking out`, '◌', 'g');
        } else {
          const rows = prRows(ov), r = rows[Math.min(ov.sel, rows.length - 1)];
          close();
          if (!r) return;
          if (r.has) { const j = vis().findIndex(w => w.branch === r.branch || w.pr === `◍ #${r.n}`); if (j >= 0) S.sel = j; return say(`#${r.n} already has a worktree — selected it`); }
          const lb = r.fork ? `pr-${r.n}/${r.branch}` : r.branch;
          const w = create(lb, { pr: `◍ #${r.n}`, prTitle: r.title });
          if (w) {
            S.prOnce = true;
            note(r.fork ? 'prfork' : 'pr', w, r); mission('pr');
            say(r.fork ? `fetching #${r.n} from a fork into ${lb}` : `checking out #${r.n}`, '◌', 'g');
            setCap('A teammate’s PR, one keypress away.', r.fork ? `git fetch origin refs/pull/${r.n}/head:${lb}   (pando does this)` : `pando new ${lb}`);
          }
        }
        return;
      }
      if (k.length === 1 && /[\w./#@ -]/.test(k)) { ov[field] = (ov[field] + k).slice(0, 40); ov.sel = 0; return; }
      return;
    }
    if (ov.type === 'share' || ov.type === 'unshare' || ov.type === 'remove' || ov.type === 'stopall' || ov.type === 'switch') {
      if (k === 'esc' || k === 'q') return close();
      if (k === 'y' || (ov.type === 'remove' && k === 'F')) {
        close();
        if (ov.type === 'share') share(wt);
        else if (ov.type === 'unshare') { wt.public = false; S.chat = null; say(`${wt.branch}: its public URL is closed`, '✓', 'gr'); setCap('Unshared. The link is gone.', `pando unshare ${wt.branch}`); }
        else if (ov.type === 'remove') remove(wt);
        else if (ov.type === 'stopall') { ov.up.forEach(w => { w.st = 'stopped'; w.public = false; }); S.chat = null; say(`stopped ${ov.up.length} worktrees`, '✓', 'gr'); setCap('Everything stopped.', 'pando stop --all'); }
        else if (ov.type === 'switch') start(wt, ov.to);
      }
    }
  }
  function newRows(ov) {
    const q = ov.text.trim();
    const rows = [];
    const known = vis().filter(w => !w.main).map(w => ({ branch: w.branch, has: true, note: 'has a worktree · ⏎ selects it' }));
    const main = { branch: 'main', has: true, note: 'checked out in the main checkout' };
    if (q) {
      if (!known.some(r => r.branch === q) && q !== 'main') rows.push({ branch: q, note: 'new branch' });
      known.concat([main]).filter(r => r.branch.includes(q)).forEach(r => rows.push(r));
    } else {
      rows.push(...known, main);
      if (!vis().some(w => w.branch === 'fix/login-loop')) rows.unshift({ branch: 'fix/login-loop', note: 'on origin · ⏎ checks it out' });
    }
    return rows;
  }
  function prRows(ov) {
    const q = ov.q.toLowerCase();
    return PRS.map(r => Object.assign({}, r, { has: vis().some(w => w.branch === r.branch || w.pr === `◍ #${r.n}`) }))
      .filter(r => !q || `#${r.n} ${r.title} ${r.branch} ${r.who}`.toLowerCase().includes(q));
  }
  function copy(text, ok) {
    const p = navigator.clipboard && window.isSecureContext ? navigator.clipboard.writeText(text) : Promise.reject(new Error('insecure'));
    p.then(() => say(ok, '✓', 'gr'), () => say(`${ok.replace(/^copied /, '')} — copying needs a secure page; select it by hand`, '✗', 'rd'));
  }

  /* ---------- the simulation clock ---------- */
  function tick(t) {
    for (const wt of S.wts) {
      if (wt.gone) continue;
      if (wt.busy && t >= wt.busy.until) {
        wt.busy = null; wt.changed = t;
        wt.logs.push({ src: 'install', text: 'Done in 1.4s', lvl: '' });
        say(`created ${wt.branch} — s starts it`, '✓', 'gr');
      }
      if (wt.st === 'starting' && t >= wt.readyAt) {
        wt.st = 'running'; wt.since = t; wt.changed = t;
        if (wt.mode === 'isolated') { addLog(wt, 'api', `connected to mariadb at localhost:${wt.ports.api + 2}`); }
        if (wt.mode === 'namespaced') { addLog(wt, 'api', 'connected to my_app__' + wt.dir.replace(/[^a-z0-9]+/gi, '_').toLowerCase()); }
        addLog(wt, 'api', `listening on ${wt.ports.api}`);
        addLog(wt, 'web', `VITE_API_URL=http://localhost:${wt.ports.api}`);
        addLog(wt, 'web', `listening on ${wt.ports.web}`);
        say(`${wt.branch} is ready — ${url(wt)}`, '✓', 'gr');
      }
      if (wt.st === 'running' && t >= wt.nextLog) {
        const src = h32(wt.branch, Math.floor(t * 3)) < 0.6 ? 'api' : 'web';
        const L = LINES[src];
        const j = Math.floor(h32(wt.branch + src, Math.floor(t * 7)) * L.length);
        if (t - wt.since > 0.4) addLog(wt, src, L[j]);
        const fast = S.ov && S.ov.type === 'logs' && cur() === wt;
        wt.nextLog = t + (fast ? 0.28 : 0.9) + h32(wt.branch, Math.floor(t)) * (fast ? 0.35 : 1.4);
        S.dirty = true;
      }
    }
    S.wts = S.wts.filter(w => !w.gone || t - w.gone < 1.6);
    if (S.armed && t - S.armed.t > 3) { S.armed = null; S.dirty = true; }
  }

  /* ---------- the character grid ---------- */
  function Grid(w, h) { this.w = w; this.h = h; this.ch = new Array(w * h).fill(' '); this.cl = new Array(w * h).fill(''); this.bg = new Array(w * h).fill(''); }
  Grid.prototype.put = function (x, y, s, cls) {
    if (y < 0 || y >= this.h) return x;
    for (const c of s) { if (x >= 0 && x < this.w) { const i = y * this.w + x; this.ch[i] = c; this.cl[i] = cls || ''; } x++; }
    return x;
  };
  Grid.prototype.segs = function (x, y, segs, maxW) {
    let left = maxW == null ? this.w - x : maxW;
    for (const [s, cls] of segs) {
      if (left <= 0) break;
      const cs = Array.from(s);
      const take = cs.length > left ? cs.slice(0, Math.max(0, left - 1)).join('') + '…' : s;
      x = this.put(x, y, take, cls);
      left -= Math.min(cs.length, left);
    }
    return x;
  };
  Grid.prototype.band = function (x, y, w, bg) { for (let k = 0; k < w; k++) { const xx = x + k; if (xx >= 0 && xx < this.w && y >= 0 && y < this.h) this.bg[y * this.w + xx] = bg; } };
  Grid.prototype.clear = function (x, y, w, h, bg) { for (let yy = y; yy < y + h; yy++) for (let xx = x; xx < x + w; xx++) if (xx >= 0 && xx < this.w && yy >= 0 && yy < this.h) { const i = yy * this.w + xx; this.ch[i] = ' '; this.cl[i] = ''; this.bg[i] = bg || ''; } };
  Grid.prototype.box = function (x, y, w, h, title, cls) {
    cls = cls || 'bd';
    this.put(x, y, '╭', cls);
    let xx = this.put(x + 1, y, ' ', cls);
    xx = this.segs(xx, y, title, w - 4);
    xx = this.put(xx, y, ' ', cls);
    this.put(xx, y, '─'.repeat(Math.max(0, x + w - 1 - xx)), cls);
    this.put(x + w - 1, y, '╮', cls);
    for (let yy = y + 1; yy < y + h - 1; yy++) { this.put(x, yy, '│', cls); this.put(x + w - 1, yy, '│', cls); }
    this.put(x, y + h - 1, '╰' + '─'.repeat(w - 2) + '╯', cls);
  };
  Grid.prototype.html = function () {
    const esc = c => (c === '&' ? '&amp;' : c === '<' ? '&lt;' : c === '>' ? '&gt;' : c);
    let out = '';
    for (let y = 0; y < this.h; y++) {
      let line = '', run = '', key = null;
      for (let x = 0; x < this.w; x++) {
        const i = y * this.w + x, k = (this.cl[i] + ' ' + this.bg[i]).trim();
        if (k !== key) { if (run) line += key ? `<span class="${key}">${run}</span>` : run; run = ''; key = k; }
        run += esc(this.ch[i]);
      }
      if (run) line += key ? `<span class="${key}">${run}</span>` : run;
      out += `<span class="ln">${line}</span>`;
    }
    return out;
  };

  /* ---------- the main screen ---------- */
  const pad = (s, n) => { const c = Array.from(s); return c.length >= n ? c.slice(0, n).join('') : s + ' '.repeat(n - c.length); };
  const padL = (s, n) => { const c = Array.from(s); return c.length >= n ? c.slice(0, n).join('') : ' '.repeat(n - c.length) + s; };
  function draw(G, t) {
    const W = G.w, H = G.h;
    header(G, t);
    footer(G);
    const top = 1, bh = H - 2;
    if (W >= 72) {
      const lw = Math.round(W * 0.6);
      list(G, 0, top, lw, bh, t);
      detail(G, lw, top, W - lw, bh, t);
    } else {
      const lh = Math.min(bh - 9, vis().length + 5);
      list(G, 0, top, W, lh, t);
      detail(G, 0, top + lh, W, bh - lh, t);
    }
    if (S.ov) overlay(G, S.ov, t);
  }
  function header(G, t) {
    const m = S.msg;
    const busy = vis().find(w => w.busy);
    if (busy && !(m && t - m.t < 1.2)) {
      G.segs(0, 0, [[' ', ''], [SPIN[P.reduced ? 0 : Math.floor(t * 10) % 10], 'g sp'], [` ${busy.busy.what} ${busy.branch} · ${busy.busy.stage} ${age(t - busy.busy.t)}`, '']]);
      return;
    }
    if (m && t - m.t < 4.5) { G.segs(0, 0, [[' ' + m.icon, m.cls], [' ' + m.text, '']]); return; }
    const up = vis().filter(w => w.st !== 'stopped').length, n = vis().filter(w => !w.main).length;
    G.segs(0, 0, [[' pando', 'w b'], [' my-app', ''], [' · ', 'd'], ['main', ''], [' · ', 'd'], [`${n} worktrees  `, ''], ['●', 'gr'], [` ${up} running`, ''], [' · shared: ', 'd'], ['mariadb ', ''], ['●', 'gr'], [' up', 'd']]);
  }
  function footer(G) {
    const wt = cur(), running = wt && wt.st !== 'stopped', pr = wt && wt.pr ? [['space p', 'open PR']] : [];
    const H = running
      ? [['j/k', 'move', 1], ['l', 'logs', 1], ['x', 'stop', 1], ['⏎', 'mode'], ['r', 'restart'], ['space g', 'git'], ...pr, ['o', 'open'], ['c', 'copy url'], ['t', 'share'], ...(wt.public ? [['O', 'public'], ['C', 'copy public']] : []), ['tab', 'process'], ['!', 'shell'], ['e', 'edit'], ['n', 'new'], ['+', 'mark'], ['/', 'filter'], ['?', 'help', 1], ['q', 'quit', 1]]
      : [['j/k', 'move', 1], ['⏎', 'start', 1], ['space g', 'git'], ...pr, ['i', 'isolated'], ['l', 'logs'], ['!', 'shell'], ['e', 'edit'], ['n', 'new'], ['d', 'remove'], ['+', 'mark'], ['/', 'filter'], ['?', 'help', 1], ['q', 'quit', 1]];
    const len = arr => arr.reduce((a, h, i) => a + h[0].length + 1 + h[1].length + (i ? 3 : 0), 1);
    const list = H.slice();
    while (len(list) > G.w) { let j = -1; for (let i = list.length - 1; i >= 0; i--) if (!list[i][2]) { j = i; break; } if (j < 0) break; list.splice(j, 1); }
    const segs = [[' ', '']];
    list.forEach((h, i) => { if (i) segs.push([' · ', 'f']); segs.push([h[0], 'w'], [' ' + h[1], 'd']); });
    G.segs(0, G.h - 1, segs);
  }
  function modeMark(wt) { return wt.main ? [' ⌂', 'g'] : wt.st !== 'stopped' && wt.mode === 'isolated' ? [' ▣', 'or'] : wt.st !== 'stopped' && wt.mode === 'namespaced' ? [' ◧', 'am'] : null; }
  function list(G, x, y, w, h, t) {
    const rows = vis();
    G.box(x, y, w, h, [[`worktrees (${rows.filter(r => !r.main).length})`, '']]);
    const iw = w - 2;
    const want = [
      { k: 'PR', w: 8, on: rows.some(r => r.pr), v: r => r.pr || '', cls: r => (r.pr ? 'gr' : '') },
      { k: 'port', w: 8, on: rows.some(r => r.st !== 'stopped'), v: r => (r.st !== 'stopped' && r.ports ? ':' + r.ports.web : ''), cls: () => '' },
      { k: 'public', w: 8, on: rows.some(r => r.public), v: r => (r.public ? '◈' : ''), cls: () => 'g' },
      { k: 'git', w: 10, on: rows.some(r => r.git), v: r => r.git, cls: () => 'd', right: true },
    ];
    let cols = want.filter(c => c.on);
    const bw = () => iw - cols.reduce((a, c) => a + c.w + 1, 0);
    for (const drop of ['port', 'public', 'PR', 'git']) if (bw() < 22) cols = cols.filter(c => c.k !== drop);
    const BW = bw();
    let xx = G.put(x + 1, y + 1, pad('     branch', BW), 'd');
    cols.forEach(c => { xx = G.put(xx, y + 1, '│', 'bd'); xx = G.put(xx, y + 1, pad(' ' + c.k, c.w), 'd'); });
    xx = G.put(x + 1, y + 2, '─'.repeat(BW), 'bd');
    cols.forEach(c => { xx = G.put(xx, y + 2, '┼', 'bd'); xx = G.put(xx, y + 2, '─'.repeat(c.w), 'bd'); });
    rows.forEach((r, i) => {
      const yy = y + 3 + i; if (yy >= y + h - 1) return;
      const sel = i === S.sel, [g, gc] = glyph(r), mark = modeMark(r);
      const word = r.busy ? 'creating' : r.st === 'starting' ? 'starting' : r.st === 'failed' ? 'failed' : '';
      const gone = r.gone ? 'f' : '';
      const segs = [[' ' + (sel ? '▸' : ' ') + ' ', 'g'], [g, gone || gc], [' ' + r.branch, gone || (sel ? 'w' : '')]];
      if (mark) segs.push(mark);
      const used = 3 + 1 + 1 + Array.from(r.branch).length + (mark ? 2 : 0);
      const room = BW - used - 1;
      if (word && room > word.length) segs.push([' '.repeat(room - word.length) + word, r.st === 'failed' ? 'rd' : 'g']);
      G.segs(x + 1, yy, segs, BW);
      let cx = x + 1 + BW;
      cols.forEach(c => { cx = G.put(cx, yy, '│', 'bd'); const v = c.v(r); cx = G.put(cx, yy, c.right ? padL(v + ' ', c.w) : pad(' ' + v, c.w), c.cls(r)); });
      if (sel) G.band(x + 1, yy, iw, 'sel');
      const fresh = t - Math.max(r.changed, r.born);
      if (fresh < 0.9 && !sel) G.band(x + 1, yy, iw, fresh < 0.3 ? 'fl1' : fresh < 0.6 ? 'fl2' : 'fl3');
    });
  }
  function detail(G, x, y, w, h, t) {
    const wt = cur(); if (!wt) return;
    const [g, gc] = glyph(wt);
    G.box(x, y, w, h, [[g, gc], [' ' + wt.branch, 'w b']]);
    const iw = w - 3, X0 = x + 2;
    const L = [];
    const up = wt.st === 'running' ? `  up ${age(t - wt.since)}` : wt.st === 'starting' ? `  for ${age(t - wt.since)}` : '';
    const stw = wt.busy ? 'creating' : wt.st;
    L.push([['status ', 'd'], [g, gc], [' ' + stw, ''], [up, 'd']]);
    ['api', 'web'].forEach((p, i) => {
      const [pg, pc] = wt.st === 'stopped' ? ['○', 'd'] : wt.st === 'starting' ? ['◌', 'g'] : ['●', 'gr'];
      const pid = 48000 + Math.floor(h32(wt.branch + p) * 9000);
      const info = wt.st === 'running' ? `running   pid ${pid}  up ${age(t - wt.since)}` : wt.st === 'starting' ? `starting  for ${age(t - wt.since)}` : 'stopped';
      L.push([[wt.proc === i ? '▸ ' : '  ', 'g'], [pg, pc], [`  ${p}  `, 'w'], [info, 'd']]);
    });
    if (wt.st !== 'stopped' && wt.mode === 'isolated') L.push([['  ', ''], [wt.st === 'running' ? '●' : '◌', wt.st === 'running' ? 'gr' : 'g'], [' mariadb   ', 'w'], [`service   port ${wt.ports.api + 2}`, 'd']]);
    if (wt.st !== 'stopped') L.push([['url    ', 'd'], [url(wt), 'w']]);
    if (wt.public) L.push([['public ', 'd'], [PUBLIC, 'g']]);
    if (wt.ports && wt.st !== 'stopped') L.push([['ports  ', 'd'], [`api ${wt.ports.api}  web ${wt.ports.web}` + (wt.mode === 'isolated' ? `  mariadb ${wt.ports.api + 2}` : ''), '']]);
    else if (wt.ports && !wt.main) L.push([['ports  ', 'd'], [`api ${wt.ports.api}  web ${wt.ports.web}`, 'd'], ['  kept while stopped', 'f']]);
    if (wt.main) L.push([['mode   ', 'd'], ['shared', ''], ['  the project\'s own services — only its processes run', 'd']]);
    else if (wt.st === 'stopped') L.push([['mode   ', 'd'], [wt.last, ''], ['  last used · ⏎ chooses', 'd']]);
    else if (wt.mode === 'isolated') L.push([['mode   ', 'd'], ['▣ isolated', 'or'], ['  private services · S shares', 'd']]);
    else if (wt.mode === 'namespaced') L.push([['mode   ', 'd'], ['◧ namespaced', 'am'], ['  its own database in the project\'s servers', 'd']]);
    else L.push([['mode   ', 'd'], ['shared', ''], ['  the project\'s services · i isolates', 'd']]);
    L.push([]);
    L.push([['git    ', 'd'], [wt.gitLong, '']]);
    L.push([['branch ', 'd'], [wt.branch, '']]);
    if (wt.pr) L.push([['pr     ', 'd'], [wt.pr, 'gr'], ['  ' + wt.prTitle, '']]);
    L.push([['path   ', 'd'], [wt.main ? '~/code/my-app' : `~/.pando/projects/${PID}/worktrees/${wt.dir}`, '']]);
    L.push([]);
    const src = ['api', 'web'][wt.proc];
    const lines = wt.logs.filter(l => l.src === src && !l.gap);
    L.push([['log  ', 'd'], [wt.proc === 0 ? '▸api' : ' api', wt.proc === 0 ? 'g' : 'd'], [' │ ', 'bd'], [wt.proc === 1 ? '▸web' : 'web', wt.proc === 1 ? 'g' : 'd'], [`  ${lines.length} lines · tab next · P restarts it`, 'd']]);
    const room = h - 2 - L.length;
    const tail = room > 0 ? lines.slice(-room) : [];
    if (!tail.length && room > 0) L.push([['(nothing logged yet)', 'f']]);
    tail.forEach(l => L.push([[l.text, l.lvl === 'E' ? 'rd' : l.lvl === 'W' ? 'g' : 'd2']]));
    L.slice(0, h - 2).forEach((segs, i) => G.segs(X0, y + 1 + i, segs, iw));
  }

  /* ---------- dialogs ---------- */
  function modal(G, w, lines, title, hl) {
    w = Math.min(w, G.w - 2);
    const h = lines.length + 2, x = Math.floor((G.w - w) / 2), y = Math.max(1, Math.floor((G.h - h) / 2));
    G.clear(x, y, w, h, 'mbg');
    G.box(x, y, w, h, [[title, 'g b']], 'gb');
    for (let yy = y; yy < y + h; yy++) G.band(x, yy, w, 'mbg');
    lines.forEach((segs, i) => { G.segs(x + 2, y + 1 + i, segs, w - 4); if (hl === i) G.band(x + 1, y + 1 + i, w - 2, 'msel'); });
  }
  function overlay(G, ov, t) {
    const wt = cur();
    if (ov.type === 'mode') {
      const running = wt.st !== 'stopped';
      const opts = [['shared', 'the main checkout\'s servers and its data'], ['namespaced (experimental)', 'its own database and slot in the main checkout\'s servers'], ['isolated', 'servers of its own, on ports of its own']];
      const key = ['shared', 'namespaced', 'isolated'];
      const lines = [[], [[`  ${running ? 'run' : 'start'} `, ''], [wt.branch, 'w b'], [' on which services?', '']], []];
      opts.forEach((o, i) => {
        const tag = running && key[i] === wt.mode ? 'running  ' : !running && key[i] === wt.last ? 'last used  ' : '';
        lines.push([[i === ov.sel ? '  ▸ ' : '    ', 'g'], [pad(o[0], 27), i === ov.sel ? 'g b' : 'w'], [tag, 'gr'], [o[1], 'd']]);
      });
      const same = running && key[ov.sel] === wt.mode;
      lines.push([], [['  ↑↓ ', 'w'], ['choose   ', 'd'], ['⏎ ', 'w'], [!running ? 'starts it' : same ? 'keeps it as it is' : 'switches it — every process restarts', 'd'], ['   esc ', 'w'], ['cancel', 'd']], []);
      return modal(G, 92, lines, 'mode', 3 + ov.sel);
    }
    if (ov.type === 'new') {
      const rows = newRows(ov); ov.sel = Math.min(ov.sel, Math.max(0, rows.length - 1));
      const lines = [[], [['  branch ', 'd'], [ov.text, 'w'], [Math.floor(t * 2) % 2 ? '▏' : ' ', 'g']], [['  new branches fork from main  ', 'd'], ['tab changes it', 'f']], []];
      rows.slice(0, 5).forEach((r, i) => lines.push([[i === ov.sel ? '  ▸ ' : '    ', 'g'], [pad(r.branch, 28), i === ov.sel ? 'w b' : ''], [padL(r.note, 32), 'd']]));
      if (!rows.length) lines.push([['    type a branch name', 'f']]);
      lines.push([], [['  ⏎ ', 'w'], ['create   ', 'd'], ['↑↓ ', 'w'], ['choose   ', 'd'], ['tab ', 'w'], ['base   ', 'd'], ['esc ', 'w'], ['cancel', 'd']], []);
      return modal(G, 68, lines, 'new worktree', 4 + ov.sel);
    }
    if (ov.type === 'prs') {
      const rows = prRows(ov); ov.sel = Math.min(ov.sel, Math.max(0, rows.length - 1));
      const lines = [[], [['  filter ', 'd'], [ov.q, 'w'], [Math.floor(t * 2) % 2 ? '▏' : ' ', 'g']], []];
      rows.forEach((r, i) => lines.push([[i === ov.sel ? '  ▸ ' : '    ', 'g'], [`#${r.n} `, 'w b'], [pad(r.title, 36), i === ov.sel ? 'w' : ''], [padL(r.has ? 'has a worktree' : r.who, 22), 'd']]));
      if (!rows.length) lines.push([['    no open pull request matches', 'f']]);
      const r = rows[ov.sel];
      lines.push([]);
      if (r) lines.push(r.has ? [['  ⏎ selects its worktree', 'd']] : r.fork ? [[`  pr-${r.n}/${r.branch}`, 'g'], ['  from a fork · ⏎ fetches it into a worktree', 'd']] : [[`  ${r.branch}`, 'g'], ['  on origin · ⏎ checks it out', 'd']]);
      lines.push([['  ⏎ ', 'w'], ['worktree   ', 'd'], ['↑↓ ', 'w'], ['choose   ', 'd'], ['esc ', 'w'], ['cancel', 'd']], []);
      return modal(G, 76, lines, 'open pull requests', 3 + ov.sel);
    }
    if (ov.type === 'share') return modal(G, 60, [[], [['  share ', ''], [wt.branch, 'w b'], [' publicly?', '']], [['  ' + url(wt), 'd']], [['  anyone with the public link reaches it — t stops it', 'd']], [], [['  y ', 'w'], ['share   ', 'd'], ['esc ', 'w'], ['cancel', 'd']], []], 'share');
    if (ov.type === 'unshare') return modal(G, 60, [[], [['  stop sharing ', ''], [wt.branch, 'w b'], ['?', '']], [['  ' + PUBLIC, 'g']], [['  anyone with that link loses it at once', 'd']], [], [['  y ', 'w'], ['stop sharing   ', 'd'], ['esc ', 'w'], ['keep it', 'd']], []], 'stop sharing');
    if (ov.type === 'remove') {
      const lines = [[], [['  remove ', ''], [wt.branch, 'w b'], ['?', '']], [['  the branch is kept; its logs and data are deleted', 'd']]];
      if (wt.st !== 'stopped') lines.push([['  ● ', 'gr'], ['it is running — removing stops it first', 'd']]);
      lines.push([], [['  y ', 'w'], ['remove   ', 'd'], ['F ', 'w'], ['force   ', 'd'], ['esc ', 'w'], ['cancel', 'd']], []);
      return modal(G, 64, lines, 'remove worktree');
    }
    if (ov.type === 'stopall') {
      const lines = [[], [[ov.up.length === 1 ? '  stop the one worktree that is up?' : `  stop all ${ov.up.length} worktrees that are up?`, '']]];
      ov.up.forEach(w => lines.push([['  ● ', 'gr'], [w.branch, '']]));
      lines.push([['  their services and public URLs go down too', 'd']], [], [['  y ', 'w'], ['stop all   ', 'd'], ['esc ', 'w'], ['cancel', 'd']], []);
      return modal(G, 56, lines, 'stop everything');
    }
    if (ov.type === 'switch') {
      const to = ov.to;
      return modal(G, 62, [[], [[`  restart `, ''], [wt.branch, 'w b'], [` ${to}?`, '']],
        [[`  it runs ${wt.mode === 'isolated' ? 'private copies of the services' : wt.mode + ' now'}`, 'd']],
        [[to === 'shared' ? (wt.mode === 'namespaced' ? '  it moves to the main checkout\'s data; its namespaces are kept until rm' : '  they stop, and it moves to the project\'s own') : '  private copies of the services start', 'd']], [['  api, web restart', 'd']], [],
        [['  y ', 'w'], [`restart ${to}   `, 'd'], ['esc ', 'w'], ['keep it as it is', 'd']], []], 'restart ' + to);
    }
    if (ov.type === 'logs') return logViewer(G, ov, t);
    if (ov.type === 'help') return help(G, ov);
    if (ov.type === 'msgs') {
      const lines = [[]].concat((S.msgs.length ? S.msgs : ['nothing yet']).slice(0, G.h - 6).map(m => [['  ' + m, '']]));
      lines.push([], [['  esc ', 'w'], ['close', 'd']]);
      return modal(G, G.w - 6, lines, 'messages');
    }
  }
  function logViewer(G, ov, t) {
    const wt = cur(), W = G.w, H = G.h;
    G.clear(0, 0, W, H, '');
    const srcs = ['all', 'api', 'web', 'install'], src = srcs[ov.tab];
    G.box(0, 0, W, H, [[`${wt.branch} · ${src}`, 'w b'], [` (${wt.st})`, 'd']]);
    let x = 2; srcs.forEach((s, i) => { x = G.put(x, 1, `${i + 1}:${s}`, i === ov.tab ? 'g b' : 'd'); x = G.put(x, 1, '   ', ''); });
    let lines = wt.logs.filter(l => !l.gap && (src === 'all' ? l.src !== 'install' : l.src === src));
    if (ov.filter === 1) lines = lines.filter(l => l.lvl);
    if (ov.filter === 2) lines = lines.filter(l => l.lvl === 'E');
    const room = H - 4, tail = lines.slice(-room);
    tail.forEach((l, i) => {
      const y = 2 + i, cls = l.lvl === 'E' ? 'rd' : l.lvl === 'W' ? 'g' : '';
      G.put(1, y, '▎', l.src === 'api' ? 'am' : l.src === 'web' ? 'gr' : 'd');
      if (src === 'all') G.segs(2, y, [[pad(l.src, 3), l.src === 'api' ? 'am' : 'gr'], [' │ ', 'bd'], [l.text, cls]], W - 4);
      else G.segs(2, y, [[l.text, cls]], W - 4);
      if (ov.jump && t - ov.jump < 1.2 && l.lvl === 'E') G.band(1, y, W - 2, 'msel');
    });
    if (!tail.length) G.put(3, 2, wt.st === 'stopped' ? '(stopped — nothing new)' : '(no output yet)', 'f');
    const E_ = lines.filter(l => l.lvl === 'E').length, W_ = lines.filter(l => l.lvl === 'W').length;
    const filt = ['', ' · warnings and up', ' · errors'][ov.filter];
    const segs = [[' j/k ', 'w'], ['move · ', 'd'], ['g/G ', 'w'], ['top/live · ', 'd'], ['/ ', 'w'], ['search · ', 'd'], ['f ', 'w'], ['filter' + filt + ' · ', 'd'], ['e ', 'w'], ['errors · ', 'd'], ['1-9 ', 'w'], ['source · ', 'd'], ['q ', 'w'], ['back  ', 'd'], [`${E_}E `, 'rd'], [`${W_}W`, 'g']];
    G.segs(1, H - 2, segs, W - 12);
    G.put(W - 10, H - 2, 'FOLLOW ', 'd'); G.put(W - 3, H - 2, '●', wt.st === 'running' ? 'gr' : 'd');
  }
  const HELP = [['j k ↓ ↑', 'move down / up'], ['g G', 'first / last'], ['⏎', 'choose its mode — shared, namespaced (experimental), isolated — and start it, or switch it when it runs'], ['s', 'start it, in the mode it last ran in'],
    ['i', 'start it isolated: private copies of the services'], ['S', 'start it shared: the project\'s own services'], ['x', 'stop it (x twice when it runs)'], ['X', 'stop everything that runs (asks first)'],
    ['r', 'restart it: r twice when it runs, once when stopped starts it'], ['P', 'restart only the ▸ process (tab picks it; P twice)'], ['o', 'open its URL in the browser'], ['t', 'share it publicly, or stop sharing (asks first)'],
    ['O', 'open its public URL'], ['c', 'copy its local URL'], ['C', 'copy its public URL'], ['y', 'copy its path'], ['!', 'a shell in it (a tmux window inside tmux)'], ['e', 'open it in $VISUAL / $EDITOR'],
    ['l', 'open the log viewer'], ['PgUp PgDn', 'scroll the log preview'], ['tab', 'preview the next process\'s log'], ['n', 'new worktree'], ['p', 'open pull requests: ⏎ makes a worktree for one'], ['d', 'remove it (never the main checkout)'],
    ['+', 'mark it, or unmark it, and move down: s x r d then act on every marked row'], ['space g', 'the git menu: fetch, pull, rebase or merge it — shows the commands first'],
    ['space p', 'open its pull request on GitHub (the row\'s #n)'], ['/', 'filter by branch or name'], [',', 'sort the list: by PR, newest first, last run first, by name (saved)'], ['m', 'messages: what pando said, in full'], ['a', 'copy the setup prompt, for your coding agent'], ['v', 'test the project\'s settings with `pando check`'],
    ['T', 'pick a colour theme'], ['R', 'refresh now'], ['?', 'this help'], ['q esc', 'quit (here: hand the keyboard back)'], ['ctrl-c', 'quit from anywhere']];
  function help(G, ov) {
    const room = G.h - 6;
    ov.scroll = Math.min(ov.scroll || 0, Math.max(0, HELP.length - room));
    const lines = [[]].concat(HELP.slice(ov.scroll, ov.scroll + room).map(([k, v]) => [['  ' + pad(k, 9), 'w b'], [v, 'd']]));
    lines.push([['  j/k scroll · esc/q/? close', 'f']]);
    return modal(G, Math.min(G.w - 4, 96), lines, 'help');
  }

  /* ---------- the grid, measured ---------- */
  let cw = 8, lh = 20, COLS = 100, ROWS = 26, lastHTML = '';
  function measure() {
    const probe = document.createElement('span');
    probe.textContent = 'M'.repeat(200);
    probe.style.cssText = 'position:absolute;visibility:hidden;white-space:pre';
    scr.appendChild(probe);
    cw = probe.getBoundingClientRect().width / 200;
    probe.remove();
    // columns from the width; rows from the height the stylesheet gives the
    // screen, so the poster it replaces had the same size and nothing moves
    const cs = getComputedStyle(scr);
    lh = parseFloat(cs.lineHeight) || cw * 2;
    const inner = scr.clientWidth - parseFloat(cs.paddingLeft) - parseFloat(cs.paddingRight);
    const tall = scr.clientHeight - parseFloat(cs.paddingTop) - parseFloat(cs.paddingBottom);
    COLS = Math.max(40, Math.floor(inner / cw));
    ROWS = Math.max(18, Math.round(tall / lh));
    S.dirty = true;
  }
  function renderTUI(t) {
    const G = new Grid(COLS, ROWS);
    draw(G, t);
    const html = G.html();
    if (html !== lastHTML) { scr.innerHTML = html; lastHTML = html; }
  }

  /* ---------- the grove below: the same state, drawn as trees ---------- */
  const SLOTX = [260, 560, 860, 1160, 1460, 1760];
  const grove = new P.Grove({ trunks: SLOTX, ghosts: 12, x0: -100, x1: 2120, seed: 3 });
  const view = new P.View(cv, { maxDpr: 2 });
  view.trackPointer(cv.parentElement);
  const G0 = grove.G;
  const vs = new Map(); // per worktree: smoothed visuals
  function vstate(wt) {
    if (!vs.has(wt)) vs.set(wt, { F: 0, green: 0, gold: 0, pocket: 0, ns: 0, shim: 0, share: 0, gone: 0, conn: 0 });
    return vs.get(wt);
  }
  const approach = (v, target, rate, dt) => v + (target - v) * (1 - Math.exp(-rate * dt));
  function drawGrove(t, dt) {
    view.resize();
    const w = view.w, h = view.h;
    let z = Math.min(w / 2050, (0.4 * h) / 345);
    if (w < 700) z = Math.max(z, w / 1500);
    view.cam = { x: w < 700 ? 900 : 1010, y: G0 - (0.6 - 0.5) * h / z, z };
    view._treePointer = view.pointerScreen ? view.unproj(view.pointerScreen[0], view.pointerScreen[1]) : null;
    view.clear();
    view.world();
    grove.drawGround(view, 1);
    const mainWt = S.wts.find(w => w.main), mainX = SLOTX[0];
    const conns = [];
    for (const wt of S.wts) {
      const v = vstate(wt), running = wt.st === 'running', starting = wt.st === 'starting' || !!wt.busy;
      v.F = approach(v.F, wt.gone ? 0 : 1, wt.gone ? 3 : 2.2, dt);
      v.green = approach(v.green, running ? 1 : 0, 4, dt);
      v.gold = starting ? 0.35 + 0.35 * Math.sin(t * 6) : approach(v.gold, 0, 5, dt);
      v.pocket = approach(v.pocket, wt.st !== 'stopped' && wt.mode === 'isolated' && !wt.gone ? 1 : 0, 2.2, dt);
      v.ns = approach(v.ns, wt.st !== 'stopped' && wt.mode === 'namespaced' && !wt.gone ? 1 : 0, 2.5, dt);
      v.conn = approach(v.conn, wt.st !== 'stopped' && wt.mode === 'shared' && !wt.main && !wt.gone ? 1 : 0, 3, dt);
      const logging = S.ov && S.ov.type === 'logs' && cur() === wt;
      v.shim = approach(v.shim, logging ? 1 : 0, 6, dt);
      const x = SLOTX[wt.slot];
      conns.push({ x, y: G0, cdx: grove.trees[wt.slot].cdx, pres: clamp(v.F * 3), green: v.green });
    }
    grove.ghosts.forEach(g => conns.push({ x: g.x, y: g.y, cdx: g.tr.cdx, pres: 0.4 }));
    grove.drawRoots(view, { alpha: 1, glow: 1 + 0.4 * S.wts.filter(w => w.st === 'running').length / 4, trunks: conns }, t);
    // the project's own services, under the main checkout: the shared database
    const nsCount = S.wts.filter(w => vstate(w).ns > 0.05).length;
    grove.drawPocket(view, mainX, 1, t, { depth: 262, noStem: false, cylinders: 1 + (nsCount ? Math.min(2, nsCount) : 0) });
    // shared worktrees drink from main's database; namespaced ones from a database of their own in it
    for (const wt of S.wts) {
      const v = vstate(wt), x = SLOTX[wt.slot];
      const k = Math.max(v.conn, v.ns);
      if (k > 0.02 && !wt.main) {
        const tx = mainX + (v.ns > 0.5 ? 36 : 0), ty = G0 + 262 - 20;
        const col = v.ns > 0.5 ? '124,255,178' : '242,184,75';
        const ctx = view.ctx;
        ctx.save(); ctx.setLineDash([6, 7]); ctx.lineDashOffset = -t * 18;
        ctx.strokeStyle = `rgba(${col},${0.55 * k})`; ctx.lineWidth = 1.6;
        ctx.beginPath(); ctx.moveTo(x, G0 + 8); ctx.bezierCurveTo(x, G0 + 150, tx + 140, ty - 40, tx + 32, ty); ctx.stroke();
        ctx.restore();
      }
      if (v.pocket > 0.01) grove.drawPocket(view, x, v.pocket, t, { depth: 262 });
    }
    if (S.celebrate) grove.drawWave(view, (t - S.celebrate) / 1.4, '124,255,178', view.unproj(0, 0)[0] - 200, view.unproj(view.w, 0)[0] + 300);
    grove.drawGhosts(view, 1, 0.22, t);
    for (const wt of S.wts) {
      const v = vstate(wt), x = SLOTX[wt.slot], sap = wt.pr && !wt.main ? 0.78 : 1;
      P.drawTree(view, grove.trees[wt.slot], { x, y: G0, s: sap, F: v.F, alpha: wt.gone ? 0.7 : 1, green: v.green, gold: v.gold, shimmer: v.shim, glow: 12 }, t);
    }
    // the share: a spark leaves the trunk for the chat
    if (S.comet && P.reduced) S.comet = null;
    if (S.comet) {
      const e = t - S.comet.t, x = SLOTX[S.comet.wt.slot];
      const r = cv.getBoundingClientRect(), cr = chat.getBoundingClientRect();
      const [sx, sy] = view.proj(x, G0 - 360);
      view.screen();
      comet(view, sx, sy, cr.left - r.left + 30, cr.top - r.top + 40, clamp(e / 0.9) + (e > 0.9 ? (e - 0.9) : 0), '242,184,75', 5, 80);
      if (e > 1.6) S.comet = null;
    }
    // labels
    view.screen();
    const size = Math.round(clamp(z * 26, 11, 20));
    S.wts.forEach((wt, i) => {
      const v = vstate(wt); if (v.F < 0.05) return;
      const [sx, sy] = view.proj(SLOTX[wt.slot], G0);
      P.drawLabel(view, sx, sy, wt.branch + (wt.main ? ' ⌂' : ''), { size, row: wt.slot % 2, alpha: clamp(v.F * 1.5) * (wt.gone ? 0.5 : 1), green: v.green, gold: v.gold > 0.3 });
    });
    // under main's database: what it is
    const [px, py] = view.proj(mainX, G0 + 262 + 70);
    const ctx = view.ctx; ctx.save(); ctx.font = `400 ${Math.round(clamp(z * 22, 11, 16))}px ${P.MONO}`; ctx.fillStyle = 'rgba(242,184,75,.8)'; ctx.textAlign = 'left';
    ctx.fillText(nsCount ? 'mariadb · the project\'s own, and a database of its own for ◧' : 'mariadb · the project\'s own', Math.max(8, px - 60), py); ctx.restore();
  }

  /* ---------- the chat a share sends its link to ---------- */
  function renderChat(t) {
    let c = S.chat;
    if (c && t - c.t > 9) { S.chat = null; c = null; }
    chat.classList.toggle('on', !!c);
    if (!c) return;
    const e = t - c.t;
    chat.querySelector('.msg.you').style.opacity = e > 0.8 ? 1 : 0;
    const reply = chat.querySelector('.msg.reply');
    reply.style.opacity = e > 1.6 ? 1 : 0;
    reply.querySelector('.body').textContent = e >= 2.6 ? 'works on my phone 👍' : 'typing…';
  }

  /* ---------- your turn: five things to try ---------- */
  const side = demo.querySelector('.missions');
  const MISSIONS = [
    { id: 'created', key: 'n', text: 'Make a worktree', how: 'n, type a branch, ⏎' },
    { id: 'started', key: '⏎', text: 'Start it, with a database of its own', how: '⏎, then ↓ ↓ for isolated, ⏎' },
    { id: 'logs', key: 'l', text: 'Read its logs', how: 'l, and esc to come back' },
    { id: 'shared', key: 't', text: 'Share it with a teammate', how: 't, then y' },
    { id: 'pr', key: 'p', text: 'Run a teammate’s pull request', how: 'p, ↓, ⏎' },
  ];
  const M = { done: {} };
  function mission(id) {
    if (!AP.user || M.done[id]) return;
    M.done[id] = now();
    renderMissions();
    if (MISSIONS.every(m => M.done[m.id])) {
      S.celebrate = now();
      setTimeout(() => showNote(['That’s pando.', 'Five keys here, five commands in your terminal. Every branch you work on, alive at once. Press <kbd>?</kbd> for every key.', 'pando new · start --isolated · logs · share', false]), 2600);
    }
  }
  function renderMissions() {
    const n = MISSIONS.filter(m => M.done[m.id]).length, nxt = MISSIONS.find(m => !M.done[m.id]);
    side.querySelector('.m-count').textContent = n === MISSIONS.length ? '✓ 5 / 5' : `${n} / ${MISSIONS.length}`;
    side.classList.toggle('all', n === MISSIONS.length);
    side.querySelector('.m-list').innerHTML = MISSIONS.map(m => {
      const st = M.done[m.id] ? 'done' : AP.user && m === nxt ? 'now' : 'todo';
      return `<li class="${st}"><span class="kbd">${m.key}</span><span class="m-t">${m.text}${st === 'now' ? `<small>${m.how}</small>` : ''}</span><span class="m-st">${st === 'done' ? '✓' : ''}</span></li>`;
    }).join('');
  }

  /* ---------- what just happened, in the real pando ---------- */
  const slug = wt => wt.dir.replace(/[^A-Za-z0-9]+/g, '_').toLowerCase();
  const NOTES = {
    created: wt => [`Made <b>${wt.branch}</b>.`, `In real pando that is <code>git worktree add</code> into <code>~/.pando/projects/…/worktrees/${wt.dir}</code>, the gitignored files you listed (like <code>.env</code>) linked in, and the project's own install run inside it. Your checkout did not change.`, `pando new ${wt.branch}`],
    started: wt => wt.mode === 'isolated'
      ? [`Started <b>${wt.branch}</b>, isolated.`, `Its processes run in the background, on ports of their own: api ${wt.ports.api}, web ${wt.ports.web}. It also got a MariaDB of its own on ${wt.ports.api + 2}, with its data under <code>~/.pando</code>.`, `pando start ${wt.branch} --isolated`]
      : wt.mode === 'namespaced'
        ? [`Started <b>${wt.branch}</b>, namespaced.`, `It keeps the main checkout's MariaDB server, with a database of its own in it: <code>my_app__${slug(wt)}</code>. Experimental.`, `pando start ${wt.branch} --namespaced`]
        : [`Started <b>${wt.branch}</b>.`, `Its processes run in the background, on ports of their own: api ${wt.ports.api}, web ${wt.ports.web}. Shared mode: it uses the main checkout's database. <kbd>⏎</kbd> again to switch.`, `pando start ${wt.branch}`],
    logs: wt => ['Its own logs.', `Every process logs to <code>~/.pando/…/logs/${wt.dir}/</code>. In here, <kbd>1</kbd>–<kbd>4</kbd> switch source, <kbd>f</kbd> filters by level, <kbd>esc</kbd> goes back.`, `pando logs ${wt.branch} -f`],
    shared: wt => ['Shared. No staging, no deploy.', 'A cloudflared quick tunnel gives it a public https URL, with no account. Anyone with the link can open it, and <kbd>t</kbd> again takes it down.', `pando share ${wt.branch}`],
    prfork: (wt, r) => [`Checked out #${r.n}.`, `pando listed the open pull requests through the GitHub CLI. This one comes from a fork, so it fetched <code>refs/pull/${r.n}/head</code> into a branch of its own, <b>${wt.branch}</b>. <kbd>s</kbd> starts it.`, 'p in the TUI', true],
    pr: (wt, r) => [`Checked out #${r.n}.`, `pando listed the open pull requests through the GitHub CLI and checked out its branch, <b>${wt.branch}</b>, from origin. <kbd>s</kbd> starts it.`, 'p in the TUI', true],
    stopped: wt => ['Stopped.', 'SIGTERM to its whole process group, SIGKILL after five seconds. Its ports stay its own, so its URL is the same next time.', `pando stop ${wt.branch}`],
    removed: wt => ['Removed.', 'Stopped first, then <code>git worktree remove</code>, then its logs and data deleted. The branch is kept.', `pando rm ${wt.branch}`],
    main: () => ['The main checkout runs too.', 'pando runs only its processes, on ports it gives them, on the project\'s own services: no install, no hooks.', 'pando start main'],
  };
  const whatEl = side.querySelector('.m-what');
  function note(ev, wt, extra) { if (NOTES[ev]) showNote(NOTES[ev](wt, extra)); }
  function showNote([title, body, cmd, plain]) {
    whatEl.querySelector('.m-what-b').innerHTML = `${title} ${body}`;
    const c = whatEl.querySelector('.m-what-c');
    c.textContent = cmd || ''; c.classList.toggle('plain', !!plain);
  }

  /* ---------- the key to press next, lit in the key bar ---------- */
  function suggest() {
    const ov = S.ov && S.ov.type;
    if (ov === 'mode') return S.ov.sel < 2 ? '↓' : '⏎';
    if (ov === 'new' || ov === 'prs') return ov === 'prs' && S.ov.sel === 0 ? '↓' : '⏎';
    if (ov === 'share' || ov === 'remove' || ov === 'stopall' || ov === 'switch' || ov === 'unshare') return 'y';
    if (ov) return 'esc';
    const wt = cur();
    if (!M.done.created) return 'n';
    if (wt && wt.busy) return null;
    if (!M.done.started) return wt && !wt.main && wt.st === 'stopped' ? '⏎' : 'j';
    if (!M.done.logs) return wt && wt.st !== 'stopped' ? 'l' : '⏎';
    if (!M.done.shared) return wt && wt.st === 'running' ? 't' : wt && wt.st === 'starting' ? null : '⏎';
    if (!M.done.pr) return 'p';
    return '?';
  }

  /* ---------- the autoplay: only if nobody plays, and never by itself with reduced motion ---------- */
  const SCRIPT = [
    [0.4, 'n'], [0.5, 'type:fix/login-loop'], [0.5, '⏎'], [2.4, 'sel:fix/login-loop'], [0.2, '⏎'], [0.7, '↓'], [0.45, '↓'], [0.6, '⏎'],
    [3.4, 'l'], [3.2, 'esc'], [0.9, 't'], [1.0, 'y'], [3.6, 'p'], [0.9, '↓'], [0.8, '⏎'], [2.6, 's'], [3.2, 'x'], [0.5, 'x'], [2.6, 'reset'],
  ];
  const IDLE = 1.5; // seconds with most of the frame in view, and no key, before it plays itself
  const AP = { on: !P.reduced, paused: false, started: false, i: 0, next: 0, user: false, typing: null, seen: 0 };
  let ratio = 0;
  new IntersectionObserver(es => es.forEach(e => { ratio = e.intersectionRatio; }), { threshold: [0, 0.3, 0.6, 1] }).observe(demo);
  function autoplay(t) {
    if (!AP.on || AP.user || AP.paused) return;
    if (!AP.started) {
      if (ratio >= 0.6) { if (!AP.seen) AP.seen = t; if (t - AP.seen > IDLE) { AP.started = true; AP.next = 0; } }
      else AP.seen = 0;
      return;
    }
    if (AP.typing) {
      if (t >= AP.typing.next) {
        key(AP.typing.s[AP.typing.k++]); flashKey(null);
        AP.typing.next = t + 0.065;
        if (AP.typing.k >= AP.typing.s.length) AP.typing = null;
      }
      return;
    }
    if (!AP.next) AP.next = t + SCRIPT[0][0];
    if (t < AP.next) return;
    const [, act] = SCRIPT[AP.i];
    if (act.startsWith('type:')) AP.typing = { s: act.slice(5), k: 0, next: t };
    else if (act.startsWith('sel:')) { const j = vis().findIndex(w => w.branch === act.slice(4)); if (j >= 0) S.sel = j; S.dirty = true; }
    else if (act === 'reset') { reset(); vs.clear(); }
    else { key(act); flashKey(act); }
    AP.i = (AP.i + 1) % SCRIPT.length;
    AP.next = t + SCRIPT[AP.i][0];
  }
  // The first key anyone presses makes the screen theirs: whatever the demo
  // did is put back first, so the five things to try start from the top.
  function takeOver() {
    if (AP.user) return;
    if (AP.started) { reset(); vs.clear(); S.chat = null; }
    AP.user = true; AP.started = false; AP.typing = null;
    renderMissions(); setPP();
  }

  /* ---------- Pause / Play and Start over, above the frame ---------- */
  const ctl = document.querySelector('.demo-ctl');
  const pp = ctl && ctl.querySelector('.pp');
  function setPP() {
    if (!pp) return;
    const moving = P.reduced ? AP.started && !AP.paused : !AP.paused;
    pp.textContent = moving ? 'Pause' : P.reduced ? 'Play demo' : 'Play';
  }
  if (ctl) ctl.addEventListener('click', e => {
    const b = e.target.closest('button'); if (!b) return;
    if (b === pp) {
      if (P.reduced) {
        if (AP.started && !AP.paused) { AP.paused = true; if (lp) lp.kick(0); }
        else { AP.on = true; AP.paused = false; AP.user = false; AP.started = true; AP.i = 0; AP.next = 0; reset(); vs.clear(); if (lp) lp.kick(36e5); }
      } else {
        AP.paused = !AP.paused;
        if (lp) lp.pause(AP.paused);
      }
      setPP();
      return;
    }
    if (b.classList.contains('rst')) {
      takeOver(); reset(); vs.clear(); M.done = {}; S.celebrate = 0; renderMissions(); kickLoop();
      showNote(['Back to the start.', 'Press <kbd>n</kbd> to make a worktree.', '', false]);
    }
  });

  /* ---------- the key bar ---------- */
  // On a phone the first ten keys show, and "More keys" the rest.
  const BAR = [['n', 'new'], ['⏎', 'mode'], ['s', 'start'], ['l', 'logs'], ['t', 'share'], ['y', 'yes'], ['p', 'PRs'], ['x', 'stop'], ['↓', 'down'], ['esc', 'back'],
    ['↑', 'up'], ['r', 'restart'], ['d', 'remove'], ['tab', 'process'], ['?', 'help']];
  function buildBar() {
    keysEl.innerHTML = BAR.map(([k, l], i) => `<button type="button"${i ? ' tabindex="-1"' : ''} data-k="${k}"${i > 9 ? ' class="xk"' : ''} aria-label="${k === '⏎' ? 'enter' : k}: ${l}"><span class="kbd">${k}</span><span>${l}</span></button>`).join('')
      + '<button type="button" tabindex="-1" class="more-k" aria-expanded="false">More keys</button>';
    // one tab stop for the whole bar; the arrows move along it
    keysEl.addEventListener('keydown', e => {
      const d = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
      if (!d && e.key !== 'Home' && e.key !== 'End') return;
      const bs = Array.from(keysEl.querySelectorAll('button')).filter(b => b.offsetParent);
      let i = bs.indexOf(document.activeElement);
      i = e.key === 'Home' ? 0 : e.key === 'End' ? bs.length - 1 : (i + d + bs.length) % bs.length;
      e.preventDefault();
      bs.forEach(b => { b.tabIndex = b === bs[i] ? 0 : -1; });
      bs[i].focus();
    });
    keysEl.addEventListener('click', e => {
      const b = e.target.closest('button'); if (!b) return;
      if (b.classList.contains('more-k')) { const on = keysEl.classList.toggle('all'); b.setAttribute('aria-expanded', String(on)); b.textContent = on ? 'Fewer keys' : 'More keys'; return; }
      takeOver(); key(b.dataset.k); flashKey(b.dataset.k); kickLoop();
      if (e.detail > 0) scr.focus({ preventScroll: true }); // a click; a keyboard press keeps its place
    });
  }
  let hot = null;
  function flashKey(k) { hot = { k, t: now() }; }
  function renderBar(t) {
    const sug = AP.user ? suggest() : 'n';
    keysEl.querySelectorAll('button[data-k]').forEach(b => {
      const k = b.dataset.k;
      b.classList.toggle('next', k === sug && !AP.started);
      b.querySelector('.kbd').classList.toggle('hot', !!hot && hot.k === k && t - hot.t < 0.5);
    });
  }

  /* ---------- keyboard: only while the screen has focus ---------- */
  const MAP = { Enter: '⏎', Escape: 'esc', ArrowUp: '↑', ArrowDown: '↓', Backspace: 'backspace' };
  function onKey(e) {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === 'Tab') return; // Tab always moves focus on
    if ((e.key === 'Escape' || e.key === 'q') && (!AP.user || (!S.ov && !S.armed))) {
      e.preventDefault();
      if (AP.user) say('q quits the real one; here it hands the keyboard back'); else tell('The keyboard is back with the page');
      const b = keysEl.querySelector('button[tabindex="0"]') || keysEl.querySelector('button'); if (b) b.focus();
      kickLoop();
      return;
    }
    const k = MAP[e.key] || (e.key.length === 1 ? e.key : null);
    if (!k) return;
    e.preventDefault(); // space included: here it is the leader key, never a page scroll
    takeOver(); key(k); flashKey(k); kickLoop();
  }
  scr.addEventListener('keydown', onKey);
  scr.addEventListener('pointerdown', () => { scr.focus({ preventScroll: true }); });

  /* ---------- the missions fold on a phone ---------- */
  const det = side.querySelector('details'), small = matchMedia('(max-width: 734px)');
  if (det) {
    // beside the screen the list is always open, so its summary is not a control there
    const sum = det.querySelector('summary'), fit = () => { sum.tabIndex = small.matches ? 0 : -1; if (!small.matches) det.open = true; };
    sum.addEventListener('click', e => { if (!small.matches) e.preventDefault(); });
    small.addEventListener('change', fit);
    fit();
  }

  /* ---------- run ---------- */
  let tuiAt = 0, lastT = 0, lp = null;
  // after a key, draw at full rate for a moment; long enough, when nothing else
  // moves, for what the key started to finish
  function kickLoop() { if (lp) lp.kick(P.reduced || AP.paused ? 6000 : 1500); }
  reset();
  buildBar();
  renderMissions();
  setPP();
  P.fontsReady.then(() => {
    measure();
    new ResizeObserver(() => { measure(); view.resize(); if (lp) lp.redraw(); }).observe(scr.parentElement);
    window.addEventListener('pando:fonts', measure);
    lp = P.loop(demo, () => {
      const t = now(), dt = lastT ? Math.min(0.1, t - lastT) : 0.016; lastT = t;
      autoplay(t);
      tick(t);
      if (S.dirty || t - tuiAt > 0.25) { renderTUI(t); tuiAt = t; S.dirty = false; }
      renderChat(t); renderBar(t);
      drawGrove(P.reduced ? 0 : t, dt);
    }, { busy: () => !!S.comet || (!!S.celebrate && now() - S.celebrate < 2) });
  });
})();
