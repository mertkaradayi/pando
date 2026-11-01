/* pando · the page: the chapter marker, the menu, copy buttons, tabs, the
 * headlines that type themselves in, the demo loaded a screen ahead, the
 * setup screen, and the docs' contents. Nothing here moves on its own for
 * more than a moment, and nothing moves at all with reduced motion. */
(function () {
  'use strict';
  const P = (window.PANDO = window.PANDO || {});
  const root = document.documentElement;
  root.classList.add('js');
  const reduced = (P.reduced = P.reduced || !!(window.matchMedia && matchMedia('(prefers-reduced-motion: reduce)').matches));
  const isDocs = document.body.classList.contains('docs-page');

  /* ---------- the chapter marker ---------- */
  const hud = document.getElementById('hud');
  const chap = hud && hud.querySelector('.chap-line');
  const nEl = chap && chap.querySelector('.chap-n'), nameEl = chap && chap.querySelector('.chap-name'), beatEl = chap && chap.querySelector('.chap-beat');
  const segs = hud ? Array.from(hud.querySelectorAll('.chap-bar i')) : [];
  const fill = (s, f) => { f = String(Math.round(f * 1000) / 1000); if (s._f !== f) { s._f = f; s.style.setProperty('--f', f); } };
  let shown = '';
  // the new name is written whole, at once: a short dim, never a scramble
  function setChapter(n, name, beat) {
    if (!chap) return;
    const key = n + '|' + name + '|' + beat;
    if (key === shown) return;
    const first = !shown;
    shown = key;
    const write = () => { nEl.textContent = n; nameEl.textContent = name; beatEl.textContent = beat ? '// ' + beat : ''; };
    if (first || reduced) { write(); return; }
    chap.classList.add('swap');
    write();
    requestAnimationFrame(() => requestAnimationFrame(() => chap.classList.remove('swap')));
  }

  const navLinks = Array.from(document.querySelectorAll('.nav-list a[href^="#"]'));
  const mark = (a, on) => { if (!!a._on === on) return; a._on = on; if (on) a.setAttribute('aria-current', 'true'); else a.removeAttribute('aria-current'); };
  const chapters = Array.from(document.querySelectorAll('[data-chapter]'));
  const beatsIn = new Map(chapters.map(c => [c, Array.from(c.querySelectorAll('[data-beat]'))]));
  const storyBeats = Array.from(document.querySelectorAll('.story .beat'));
  const story = storyBeats.length ? storyBeats[0].closest('.story') : null;

  /* ---------- the story: which beat is on, and whether its words show ---------- */
  // One rule for the grove (story.js), the words and the chapter marker: a
  // beat is on once its words have come up to where they rest, under the
  // bar. Its words show only while they rest there; the last beat's leave
  // with the grove. So words never cross the grove on their way up, and the
  // words of two beats are never on the screen together.
  const storyTexts = storyBeats.map(b => b.querySelector('.beat-text'));
  let sm = null, shownBeat = null;
  const storyMeasure = () => {
    const t = storyTexts.find(Boolean);
    if (!t) return;
    const cs = getComputedStyle(t);
    sm = { rest: parseFloat(cs.top) + parseFloat(cs.paddingTop), pads: storyTexts.map(x => (x ? parseFloat(getComputedStyle(x).paddingTop) : 0)) };
  };
  P.storyNow = function () {
    if (!sm) storyMeasure();
    if (!sm) return { cur: 0, shown: 0 };
    const near = sm.rest + Math.min(80, innerHeight * 0.08), far = sm.rest - Math.min(64, innerHeight * 0.07), last = storyTexts.length - 1;
    let cur = 0, y = 0;
    for (let i = 1; i <= last; i++) {
      if (!storyTexts[i]) continue;
      const yi = storyTexts[i].getBoundingClientRect().top + sm.pads[i];
      if (yi <= near) { cur = i; y = yi; }
    }
    return { cur, shown: cur === 0 || cur === last || y >= far ? cur : -1 };
  };
  window.addEventListener('resize', () => { sm = null; });
  window.addEventListener('pando:fonts', () => { sm = null; queue(); });

  function onScroll() {
    if (hud && !isDocs) hud.classList.toggle('top', window.scrollY < 8);
    if (isDocs) return docsScroll();
    const line = window.innerHeight * 0.32;
    let active = null, beat = '';
    const sr = story && story.getBoundingClientRect();
    const now = sr && sr.bottom > 0 ? P.storyNow() : null;
    if (now && shownBeat !== now.shown) {
      shownBeat = now.shown;
      storyBeats.forEach((b, i) => b.classList.toggle('on', i === now.shown));
    }
    // the story's beats first
    if (now && sr.bottom > line) {
      const b = storyBeats[now.cur];
      setChapter(b.dataset.chap, b.dataset.name, b.dataset.tag);
      const k = now.cur, n = parseInt(b.dataset.chap, 10);
      segs.forEach((s, i) => fill(s, i < n - 1 ? 1 : i === n - 1 ? Math.max(0.15, (k + 1) / storyBeats.length) : 0));
      navLinks.forEach(a => mark(a, false));
      return;
    }
    for (const c of chapters) if (c.getBoundingClientRect().top <= line) active = c;
    if (!active) return;
    beat = active.dataset.beat0 || '';
    for (const b of beatsIn.get(active)) if (b.getBoundingClientRect().top <= line) beat = b.dataset.beat;
    const r = active.getBoundingClientRect();
    setChapter(active.dataset.chapter, active.dataset.name, beat);
    const idx = parseInt(active.dataset.chapter, 10) - 1;
    segs.forEach((s, i) => fill(s, i < idx ? 1 : i === idx ? Math.max(0.06, Math.min(1, (line - r.top) / Math.max(1, r.height))) : 0));
    navLinks.forEach(a => mark(a, a.hash === '#' + active.id));
  }
  let ticking = false;
  const queue = () => { if (!ticking) { ticking = true; requestAnimationFrame(() => { ticking = false; onScroll(); }); } };
  window.addEventListener('scroll', queue, { passive: true });
  window.addEventListener('resize', queue);

  /* ---------- the menu on narrow screens ---------- */
  const nav = document.getElementById('nav'), toggle = nav && nav.querySelector('.nav-toggle');
  if (toggle) {
    const list = nav.querySelector('.nav-list');
    const items = () => Array.from(list.querySelectorAll('a')).concat(Array.from(nav.querySelectorAll('.nav-cta')), [toggle]);
    const setOpen = (o, focusBack) => {
      nav.classList.toggle('open', o);
      root.classList.toggle('menu-open', o);
      toggle.setAttribute('aria-expanded', String(o));
      toggle.textContent = o ? 'close' : 'menu';
      if (o) { const a = list.querySelector('a'); if (a) a.focus(); } else if (focusBack) toggle.focus();
    };
    toggle.addEventListener('click', () => setOpen(!nav.classList.contains('open'), true));
    list.addEventListener('click', e => { if (e.target.closest('a')) setOpen(false, false); });
    document.addEventListener('keydown', e => {
      if (!nav.classList.contains('open')) return;
      if (e.key === 'Escape') { e.preventDefault(); setOpen(false, true); return; }
      if (e.key !== 'Tab') return;
      // Tab and Shift+Tab go round the open menu, the toggle included
      const f = items().filter(el => el.offsetParent !== null);
      const i = f.indexOf(document.activeElement);
      e.preventDefault();
      f[(i + (e.shiftKey ? -1 : 1) + f.length) % f.length].focus();
    });
    matchMedia('(min-width: 1100px)').addEventListener('change', e => { if (e.matches && nav.classList.contains('open')) setOpen(false, false); });
  }

  /* ---------- a click on an in-page link glides there ---------- */
  // Never a global smooth scroll: a link from another page would fly the
  // whole page past. A jump settles every headline at once (below).
  let jumpUntil = 0;
  const jumping = () => performance.now() < jumpUntil;
  document.addEventListener('click', e => {
    const a = e.target.closest && e.target.closest('a[href^="#"]');
    if (!a || e.defaultPrevented || e.metaKey || e.ctrlKey || e.shiftKey || e.button) return;
    const id = decodeURIComponent(a.hash.slice(1));
    const t = id ? document.getElementById(id) : null;
    if (!t) return;
    e.preventDefault();
    jumpUntil = performance.now() + 1800;
    settleAll();
    const y = t.getBoundingClientRect().top + window.scrollY - (parseFloat(getComputedStyle(root).scrollPaddingTop) || 0);
    const far = Math.abs(y - window.scrollY) > innerHeight * 4;
    window.scrollTo({ top: Math.max(0, y), behavior: reduced || far ? 'auto' : 'smooth' });
    if (history.pushState) history.pushState(null, '', '#' + id);
    if (!t.hasAttribute('tabindex') && !/^(A|BUTTON|INPUT|SELECT|TEXTAREA)$/.test(t.tagName)) t.setAttribute('tabindex', '-1');
    t.focus({ preventScroll: true });
  });
  window.addEventListener('hashchange', () => { jumpUntil = performance.now() + 1200; settleAll(); });

  /* ---------- headlines type themselves in, the real letters only ---------- */
  // Each letter is a span from the start, so the text never changes and its
  // size never moves. Only a headline that is below the screen when the page
  // opens is armed; it types when it comes up, in under a second, unless the
  // reader jumped there, which shows it whole at once.
  const heads = Array.from(document.querySelectorAll('.headline[data-type]'));
  function split(el) {
    let i = 0;
    const walk = node => {
      Array.from(node.childNodes).forEach(c => {
        if (c.nodeType === 3) {
          const f = document.createDocumentFragment();
          for (const ch of c.textContent) {
            if (ch === ' ') { f.appendChild(document.createTextNode(' ')); continue; }
            const s = document.createElement('span'); s.className = 'ch'; s.style.setProperty('--i', i++); s.textContent = ch; f.appendChild(s);
          }
          c.replaceWith(f);
        } else if (c.nodeType === 1 && !c.classList.contains('cur') && !c.classList.contains('chev')) walk(c);
      });
    };
    const txt = el.querySelector('.txt');
    if (txt) walk(txt);
    return i;
  }
  function settle(h) { h.classList.remove('armed', 'typing'); h.classList.add('typed'); }
  function blink(h) { if (!reduced) { h.classList.add('blink'); setTimeout(() => h.classList.remove('blink'), 3400); } }
  function settleAll() { heads.forEach(h => { if (h.classList.contains('armed')) settle(h); }); }
  if (!reduced && 'IntersectionObserver' in window) {
    const hio = new IntersectionObserver(es => es.forEach(e => {
      if (!e.isIntersecting) return;
      const h = e.target;
      hio.unobserve(h);
      if (!h.classList.contains('armed')) { blink(h); return; }
      if (jumping()) { settle(h); blink(h); return; }
      const n = +h.dataset.n || 0;
      h.classList.remove('armed'); h.classList.add('typing');
      setTimeout(() => { settle(h); blink(h); }, n * 15 + 90);
    }), { rootMargin: '0px 0px -10% 0px' });
    heads.forEach(h => {
      h.dataset.n = split(h);
      const r = h.getBoundingClientRect();
      if (r.top > innerHeight && !location.hash) h.classList.add('armed'); else h.classList.add('typed');
      hio.observe(h);
    });
    // a page opened at an anchor, or restored mid-way, shows every headline whole
    if (location.hash) settleAll();
    window.addEventListener('pageshow', e => { if (e.persisted) settleAll(); });
  }
  const tagline = document.querySelector('.tagline .cur');
  if (tagline) blink(tagline.parentElement);

  /* ---------- copy buttons ---------- */
  document.querySelectorAll('.copy:empty').forEach(b => { b.textContent = 'copy'; if (!b.hasAttribute('aria-label')) b.setAttribute('aria-label', 'Copy the command'); });
  document.addEventListener('click', e => {
    const b = e.target.closest && e.target.closest('[data-copy]');
    if (!b) return;
    let text = b.dataset.copy;
    if (!text) { const c = b.parentElement && b.parentElement.querySelector('code'); text = c ? c.textContent : ''; }
    text = (text || '').replace(/^\$ /, '').trim();
    if (!text) return;
    const done = () => {
      if (b.dataset.was == null) b.dataset.was = b.textContent;
      b.classList.add('done'); b.textContent = 'copied';
      setTimeout(() => { b.classList.remove('done'); b.textContent = b.dataset.was; }, 1400);
    };
    if (navigator.clipboard && window.isSecureContext) navigator.clipboard.writeText(text).then(done, () => fallback(text, done, b));
    else fallback(text, done, b);
  });
  function fallback(text, done, b) {
    const ta = document.createElement('textarea'); ta.value = text; ta.setAttribute('readonly', ''); ta.style.position = 'fixed'; ta.style.opacity = '0';
    document.body.appendChild(ta); ta.select();
    try { if (document.execCommand('copy')) done(); } catch (_) { /* nothing copied: say nothing */ }
    ta.remove();
    b.focus();
  }

  /* ---------- tabs: the ways to install ---------- */
  document.querySelectorAll('[role="tablist"]').forEach(list => {
    const tabs = Array.from(list.querySelectorAll('[role="tab"]'));
    const select = (t, focus) => {
      tabs.forEach(x => {
        const on = x === t, pane = document.getElementById(x.getAttribute('aria-controls'));
        x.setAttribute('aria-selected', String(on)); x.tabIndex = on ? 0 : -1;
        if (pane) pane.hidden = !on;
      });
      if (focus) t.focus();
    };
    tabs.forEach(t => t.addEventListener('click', () => select(t, false)));
    list.addEventListener('keydown', e => {
      const i = tabs.indexOf(document.activeElement);
      if (i < 0) return;
      const d = { ArrowRight: 1, ArrowLeft: -1, Home: -i, End: tabs.length - 1 - i }[e.key];
      if (d == null) return;
      e.preventDefault();
      select(tabs[(i + d + tabs.length) % tabs.length], true);
    });
    select(tabs.find(t => t.getAttribute('aria-selected') === 'true') || tabs[0], false);
  });

  /* ---------- code that scrolls sideways can be reached by keyboard ---------- */
  const sideways = el => { if (el.scrollWidth > el.clientWidth + 1) { el.tabIndex = 0; el.setAttribute('role', 'region'); if (!el.hasAttribute('aria-label')) el.setAttribute('aria-label', 'Code, scrolls sideways'); } else if (el.getAttribute('role') === 'region') { el.removeAttribute('tabindex'); el.removeAttribute('role'); } };
  const blocks = Array.from(document.querySelectorAll('pre, .tbl-wrap')).filter(el => !el.closest('.tui, .setup'));
  const measureBlocks = () => blocks.forEach(sideways);
  window.addEventListener('resize', measureBlocks);
  window.addEventListener('pando:fonts', measureBlocks);
  requestAnimationFrame(measureBlocks);

  /* ---------- the demo: loaded about a screen before it is reached ---------- */
  const demo = document.getElementById('demo');
  if (demo && 'IntersectionObserver' in window) {
    const load = () => {
      ['assets/ports.js', 'assets/tui.js'].forEach(src => {
        const s = document.createElement('script'); s.src = src; s.async = false; document.body.appendChild(s);
      });
    };
    const dio = new IntersectionObserver(es => { if (es.some(e => e.isIntersecting)) { dio.disconnect(); load(); } }, { rootMargin: '120% 0px' });
    dio.observe(demo);
  }

  /* ---------- the setup screen: waiting, then green, once ---------- */
  const setup = document.querySelector('.setup');
  if (setup) {
    const state = setup.querySelector('.setup-state'), spin = setup.querySelector('.spin');
    const STEPS = [[0, 'waiting for the setup…'], [1.4, 'settings saved · testing them'], [2.6, 'testing: installing · starting api, web…'], [4.2, 'You’re ready to use pando in acme-shop · web answered (HTTP 200)']];
    const SP = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏';
    const finish = () => { state.textContent = STEPS[STEPS.length - 1][1]; spin.textContent = '✓'; setup.classList.add('ready'); };
    if (reduced) finish();
    else {
      const sio = new IntersectionObserver(es => {
        if (!es.some(e => e.isIntersecting)) return;
        sio.disconnect();
        if (jumping()) { finish(); return; }
        const t0 = performance.now();
        const tick = () => {
          const e = (performance.now() - t0) / 1000;
          let k = 0; STEPS.forEach((s, i) => { if (e >= s[0]) k = i; });
          if (k === STEPS.length - 1) { finish(); return; }
          if (state.textContent !== STEPS[k][1]) state.textContent = STEPS[k][1];
          spin.textContent = SP[Math.floor(e * 10) % 10];
          setTimeout(tick, 100);
        };
        tick();
      }, { threshold: 0.5 });
      sio.observe(setup);
    }
  }

  /* ---------- docs: where you are, and the contents on a narrow screen ---------- */
  const toc = document.getElementById('toc');
  let tocLinks = [], tocTargets = [];
  const bar = document.getElementById('docs-bar'), barBtn = bar && bar.querySelector('button'), barCur = bar && bar.querySelector('.cur');
  if (toc) {
    tocLinks = Array.from(toc.querySelectorAll('a[href^="#"]'));
    tocTargets = tocLinks.map(a => document.getElementById(a.hash.slice(1)));
    if (barBtn) {
      const setOpen = (o, back) => {
        root.classList.toggle('toc-open', o);
        barBtn.setAttribute('aria-expanded', String(o));
        if (o) { const a = toc.querySelector('a[aria-current]') || toc.querySelector('a'); if (a) a.focus({ preventScroll: true }); } else if (back) barBtn.focus();
      };
      barBtn.addEventListener('click', () => setOpen(!root.classList.contains('toc-open'), false));
      toc.addEventListener('click', e => { if (e.target.closest('a')) setOpen(false, false); });
      document.addEventListener('keydown', e => {
        if (!root.classList.contains('toc-open')) return;
        if (e.key === 'Escape') { e.preventDefault(); setOpen(false, true); return; }
        if (e.key !== 'Tab') return;
        const f = tocLinks.concat([barBtn]);
        const i = f.indexOf(document.activeElement);
        e.preventDefault();
        f[(i + (e.shiftKey ? -1 : 1) + f.length) % f.length].focus();
      });
      matchMedia('(min-width: 1069px)').addEventListener('change', e => { if (e.matches) setOpen(false, false); });
    }
  }
  let docCur = null;
  function docsScroll() {
    if (!toc) return;
    const line = innerHeight * 0.25;
    let k = 0;
    tocTargets.forEach((t, i) => { if (t && t.getBoundingClientRect().top <= line) k = i; });
    if (docCur === k) return;
    docCur = k;
    tocLinks.forEach((a, i) => { if (i === k) a.setAttribute('aria-current', 'true'); else a.removeAttribute('aria-current'); });
    const name = tocLinks[k].textContent;
    if (barCur) barCur.textContent = name;
    const group = tocLinks[k].closest('ul') && tocLinks[k].closest('ul').previousElementSibling;
    setChapter('docs', name, group ? group.textContent.toLowerCase() : '');
    const a = tocLinks[k], tr = toc.getBoundingClientRect(), ar = a.getBoundingClientRect();
    if (toc.scrollHeight > toc.clientHeight && (ar.top < tr.top + 40 || ar.bottom > tr.bottom - 40)) toc.scrollTop += ar.top - tr.top - tr.height / 2;
  }

  onScroll();
})();
