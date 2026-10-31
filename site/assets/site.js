/* pando · the site's one script: the menu, the section the reader is in, copy
 * buttons, the install tabs, the docs' contents, and the grove and the demo,
 * loaded only when the page is idle or they are a screen away. */
(function () {
  'use strict';
  const d = document;
  d.documentElement.classList.add('js');
  const $ = (s, el) => (el || d).querySelector(s), $$ = (s, el) => Array.from((el || d).querySelectorAll(s));
  const idle = window.requestIdleCallback || (f => setTimeout(f, 200));

  /* ---------- a sheet: the menu on a phone, the docs' contents ---------- */
  function sheet(btn, host, cls, panel) {
    if (!btn || !host || !panel) return;
    const open = () => host.classList.contains(cls);
    const set = (o, back) => {
      host.classList.toggle(cls, o); btn.setAttribute('aria-expanded', String(o)); d.body.classList.toggle('locked', o);
      if (o) { const a = $('a', panel); if (a) a.focus(); } else if (back) btn.focus();
    };
    btn.addEventListener('click', () => set(!open(), true));
    panel.addEventListener('click', e => { if (e.target.closest('a')) set(false, false); });
    d.addEventListener('keydown', e => {
      if (!open()) return;
      if (e.key === 'Escape') set(false, true);
      else if (e.key === 'Tab') { // Tab stays inside: the button, then the sheet's links
        const f = [btn].concat($$('a', panel)), i = f.indexOf(d.activeElement);
        if (e.shiftKey ? i <= 0 : i === f.length - 1) { e.preventDefault(); f[e.shiftKey ? f.length - 1 : 0].focus(); }
      }
    });
    addEventListener('resize', () => { if (open() && !btn.offsetParent) set(false, false); });
  }
  sheet($('.menu-btn'), $('#nav'), 'open', $('#nav-links'));
  sheet($('#docs-bar button'), d.body, 'toc-open', $('#toc'));

  /* ---------- the section under a line 30% down the screen ---------- */
  function spy(links, onCur) {
    const map = new Map();
    links.forEach(a => { const h = d.getElementById(a.hash.slice(1)); if (h) map.set(h.closest('section') || h, a); });
    let cur = null;
    const io = new IntersectionObserver(es => es.forEach(e => {
      const a = map.get(e.target);
      if (e.isIntersecting) { if (cur) cur.removeAttribute('aria-current'); a.setAttribute('aria-current', 'true'); cur = a; if (onCur) onCur(a); }
      else if (a === cur) { a.removeAttribute('aria-current'); cur = null; }
    }), { rootMargin: '-30% 0px -69% 0px' });
    map.forEach((a, el) => io.observe(el));
  }
  spy($$('.nav-links a[href^="#"]'));
  const barCur = $('#docs-bar .cur');
  spy($$('#toc a'), a => {
    if (barCur) barCur.textContent = a.textContent;
    const t = a.closest('.toc'); // keep the current entry in view in a sidebar that scrolls
    if (t && t.scrollHeight > t.clientHeight + 1 && (a.offsetTop < t.scrollTop || a.offsetTop > t.scrollTop + t.clientHeight - 40)) t.scrollTop = a.offsetTop - t.clientHeight / 2;
  });
  $$('.docs h2[id], .docs h3[id]').forEach(h => {
    const a = d.createElement('a');
    a.className = 'anchor'; a.href = '#' + h.id; a.textContent = '#'; a.tabIndex = -1; a.setAttribute('aria-hidden', 'true');
    h.appendChild(a);
  });

  /* ---------- copy buttons: the text in data-copy, or the code beside them ---------- */
  const live = d.createElement('p');
  live.className = 'sr-only'; live.setAttribute('aria-live', 'polite'); d.body.appendChild(live);
  const textOf = b => (b.dataset.copy || ($('code, pre', b.closest('.cmd, .code')) || {}).textContent || '').trim();
  $$('.copy').forEach(b => {
    b.innerHTML = '<svg class="cp" viewBox="0 0 24 24" aria-hidden="true"><rect x="8.5" y="8.5" width="11" height="11" rx="2.5"/><path d="M15.5 8.5v-2a2 2 0 0 0-2-2h-7a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h2"/></svg><svg class="ok" viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>';
    if (!b.hasAttribute('aria-label')) b.setAttribute('aria-label', 'Copy: ' + textOf(b));
  });
  d.addEventListener('click', e => {
    const b = e.target.closest('.copy');
    if (!b) return;
    const text = textOf(b);
    const done = () => {
      b.classList.add('done'); live.textContent = 'Copied';
      clearTimeout(b._t); b._t = setTimeout(() => { b.classList.remove('done'); live.textContent = ''; }, 1600);
    };
    if (navigator.clipboard && window.isSecureContext) navigator.clipboard.writeText(text).then(done, () => fallback(text, done, b));
    else fallback(text, done, b);
  });
  function fallback(text, done, b) { // file:// has no clipboard API
    const ta = d.createElement('textarea');
    ta.value = text; ta.style.cssText = 'position:fixed;opacity:0';
    d.body.appendChild(ta); ta.select();
    try { if (d.execCommand('copy')) done(); } catch (_) { /* nothing copied: say nothing */ }
    ta.remove(); b.focus();
  }

  /* ---------- tabs: arrows move between them, Home and End jump ---------- */
  $$('.tabs').forEach(w => {
    const tabs = $$('[role="tab"]', w);
    if (!tabs.length) return;
    w.classList.add('seg-on');
    const sel = (t, focus) => tabs.forEach(x => {
      const on = x === t;
      x.setAttribute('aria-selected', String(on)); x.tabIndex = on ? 0 : -1;
      d.getElementById(x.getAttribute('aria-controls')).hidden = !on;
      if (on && focus) x.focus();
    });
    tabs.forEach((t, i) => {
      t.addEventListener('click', () => sel(t));
      t.addEventListener('keydown', e => {
        const k = { ArrowRight: i + 1, ArrowLeft: i - 1, Home: 0, End: tabs.length - 1 }[e.key];
        if (k == null) return;
        e.preventDefault(); sel(tabs[(k + tabs.length) % tabs.length], true);
      });
    });
    sel(tabs[0]);
  });

  /* ---------- code that scrolls sideways: reached by keyboard, faded where it goes on ---------- */
  const edges = el => {
    const more = el.scrollWidth - el.clientWidth - el.scrollLeft;
    el.classList.toggle('sx-r', more > 2); el.classList.toggle('sx-l', el.scrollLeft > 2);
  };
  const scrollers = () => $$('pre, .tbl-wrap').forEach(el => {
    if (el.closest('.tui, .screen')) return;
    if (el.scrollWidth > el.clientWidth + 1) {
      el.tabIndex = 0; if (!el.hasAttribute('aria-label')) el.setAttribute('aria-label', 'Scrollable code');
      if (!el._edges) { el._edges = true; el.addEventListener('scroll', () => edges(el), { passive: true }); }
    } else el.removeAttribute('tabindex');
    edges(el);
  });
  idle(scrollers);
  let rt;
  addEventListener('resize', () => { clearTimeout(rt); rt = setTimeout(scrollers, 250); });

  /* ---------- media fades in once ---------- */
  const rv = $$('.reveal');
  if (rv.length) {
    const io = new IntersectionObserver(es => es.forEach(e => { if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); } }), { threshold: 0.15 });
    rv.forEach(el => io.observe(el));
  }

  /* ---------- the grove after the page, the demo a screen ahead ---------- */
  const got = {};
  function script(src, cb) {
    let s = got[src];
    if (!s) {
      s = got[src] = { ok: false, q: [] };
      const el = d.createElement('script');
      el.src = src; el.onload = () => { s.ok = true; s.q.forEach(f => f()); };
      d.head.appendChild(el);
    }
    if (s.ok) cb(); else s.q.push(cb);
  }
  const chain = srcs => (function next(i) { if (i < srcs.length) script(srcs[i], () => next(i + 1)); })(0);
  if ($('.hero-grove')) {
    const go = () => idle(() => chain(['assets/grove.js']));
    if (d.readyState === 'complete') go(); else addEventListener('load', go);
  }
  const demo = $('#demo');
  if (demo) {
    const io = new IntersectionObserver(es => {
      if (!es.some(e => e.isIntersecting)) return;
      io.disconnect();
      chain(['assets/grove.js', 'assets/ports.js', 'assets/tui.js']);
    }, { rootMargin: '100% 0px' });
    io.observe(demo);
  }
})();
