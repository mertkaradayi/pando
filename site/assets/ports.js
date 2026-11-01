/* pando · ports: the same arithmetic pando uses, so every port on this site is
 * one pando would really give. md5 is here because pando's ids and port
 * windows are md5-based (not for security: for stable, short names). */
(function () {
  'use strict';
  const P = (window.PANDO = window.PANDO || {});

  const S = [7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21];
  const K = new Uint32Array(64);
  for (let i = 0; i < 64; i++) K[i] = Math.floor(Math.abs(Math.sin(i + 1)) * 4294967296) >>> 0;

  function md5(bytes) {
    const len = bytes.length, nBlocks = ((len + 8) >>> 6) + 1, w = new Uint32Array(nBlocks * 16);
    for (let i = 0; i < len; i++) w[i >> 2] |= bytes[i] << ((i % 4) * 8);
    w[len >> 2] |= 0x80 << ((len % 4) * 8);
    w[nBlocks * 16 - 2] = (len * 8) >>> 0;
    w[nBlocks * 16 - 1] = Math.floor(len / 0x20000000) >>> 0;
    let a0 = 0x67452301, b0 = 0xefcdab89, c0 = 0x98badcfe, d0 = 0x10325476;
    for (let b = 0; b < nBlocks; b++) {
      let A = a0, B = b0, C = c0, D = d0;
      for (let i = 0; i < 64; i++) {
        let F, g;
        if (i < 16) { F = (B & C) | (~B & D); g = i; }
        else if (i < 32) { F = (D & B) | (~D & C); g = (5 * i + 1) % 16; }
        else if (i < 48) { F = B ^ C ^ D; g = (3 * i + 5) % 16; }
        else { F = C ^ (B | ~D); g = (7 * i) % 16; }
        F = (F + A + K[i] + w[b * 16 + g]) >>> 0;
        A = D; D = C; C = B;
        B = (B + ((F << S[i]) | (F >>> (32 - S[i])))) >>> 0;
      }
      a0 = (a0 + A) >>> 0; b0 = (b0 + B) >>> 0; c0 = (c0 + C) >>> 0; d0 = (d0 + D) >>> 0;
    }
    const out = new Uint8Array(16);
    [a0, b0, c0, d0].forEach((v, i) => { for (let j = 0; j < 4; j++) out[i * 4 + j] = (v >>> (8 * j)) & 255; });
    return out;
  }
  const enc = new TextEncoder();
  const hex = b => Array.from(b, x => x.toString(16).padStart(2, '0')).join('');
  const md5hex = s => hex(md5(enc.encode(s)));

  // src/project.rs: <directory name>-<first 8 hex of md5(canonical root)>
  function projectId(root) {
    const clean = root.replace(/\/+$/, '') || '/';
    const dir = clean.split('/').filter(Boolean).pop() || 'repo';
    return `${dir}-${md5hex(clean).slice(0, 8)}`;
  }
  // src/worktree.rs: feat/checkout → feat+checkout
  const dirName = branch => branch.replace(/\//g, '+');
  // src/ports.rs: md5(id ␟ name), first four bytes big-endian, onto 1971 windows of 8 in 17000..=32767
  const PORT_MIN = 17000, BASE_STEP = 8, BASE_COUNT = (32767 - 17000 + 1) / 8;
  function deriveBase(id, name) {
    const a = enc.encode(id), b = enc.encode(name), buf = new Uint8Array(a.length + b.length + 1);
    buf.set(a, 0); buf[a.length] = 0x1f; buf.set(b, a.length + 1);
    const d = md5(buf), num = ((d[0] << 24) | (d[1] << 16) | (d[2] << 8) | d[3]) >>> 0;
    return (num % BASE_COUNT) * BASE_STEP + PORT_MIN;
  }
  P.md5hex = md5hex; P.projectId = projectId; P.dirName = dirName; P.deriveBase = deriveBase;

  /* ---------- the widget in 05 · How it works ---------- */
  const out = document.getElementById('pw-out');
  if (!out) return;
  const root = document.getElementById('pw-root'), branch = document.getElementById('pw-branch'), roles = document.getElementById('pw-roles');
  const esc = s => s.replace(/[&<>]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c]));
  function update() {
    const id = projectId(root.value.trim() || '/Users/you/code/acme-shop');
    const br = branch.value.trim() || 'feat/login';
    const name = dirName(br);
    const rs = roles.value.split(/[\s,]+/).filter(Boolean).slice(0, 8);
    const base = deriveBase(id, name);
    const url = rs.includes('web') ? 'web' : rs[0];
    let s = `<span class="d">project </span>  ${esc(id)}\n<span class="d">worktree</span>  ${esc(name)}\n<span class="d">window  </span>  ${base} … ${base + 7}\n`;
    rs.forEach((r, i) => { s += `<span class="d">${esc(r.padEnd(8).slice(0, 8))}</span>  <span class="g">${base + i}</span>${r === url ? `  <span class="d">← http://localhost:${base + i}</span>` : ''}\n`; });
    if (!rs.length) s += '<span class="d">no roles: nothing to serve</span>\n';
    out.innerHTML = s.trimEnd();
    if (window.PANDO && window.PANDO.measureCode) window.PANDO.measureCode();
    clearTimeout(update.t);
    const live = document.getElementById('pw-live');
    if (live) update.t = setTimeout(() => { live.textContent = rs.length ? `${name}: ports ${base} to ${base + rs.length - 1}` : `${name}: no roles`; }, 600);
  }
  [root, branch, roles].forEach(el => el.addEventListener('input', update));
  update();
})();
