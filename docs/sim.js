// "Try it" simulator: the notch idles, plays music with a karaoke line and a beat-following aurora,
// morphs into a notification, and opens into a panel whose tabs grow out of their icons.
(function () {
  const notch = document.getElementById('simNotch');
  if (!notch) return;
  const hint = document.getElementById('simHint');
  const buttons = [...document.querySelectorAll('.sim-controls button')];
  const clock = document.getElementById('simClock');
  const lineEl = document.getElementById('simLine');
  const sungEl = document.getElementById('simSung');
  const canvas = document.getElementById('simAurora');
  const eq = [...document.querySelectorAll('#simEq span')];
  const img = document.getElementById('simPanelImg');
  const body = document.getElementById('simBody');
  const tabs = [...document.querySelectorAll('#simTabs button')];
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;

  let state = 'idle';
  let before = 'idle';
  let alertTimer;

  function set(s) {
    if (s !== 'alert') before = s === 'open' ? before : s;
    state = s;
    notch.className = 'sim-notch ' + s;
    buttons.forEach(b => b.classList.toggle('on', b.dataset.state === s));
    hint.style.opacity = s === 'idle' ? '1' : '0';
    if (s === 'open') openTab(current, null);
  }

  // Clock.
  function tick() {
    const d = new Date();
    clock.textContent = String(d.getHours()).padStart(2, '0') + ':' + String(d.getMinutes()).padStart(2, '0');
  }
  tick();
  setInterval(tick, 10000);

  // Controls.
  buttons.forEach(b => b.addEventListener('click', () => {
    const s = b.dataset.state;
    clearTimeout(alertTimer);
    if (s === 'alert') {
      set('alert');
      alertTimer = setTimeout(() => set(before === 'alert' ? 'idle' : before), 4200);
    } else {
      set(s);
    }
  }));
  // The notch itself: a click opens the panel, a click outside closes it.
  notch.addEventListener('click', e => {
    if (state === 'alert') { clearTimeout(alertTimer); set('open'); return; }
    if (state !== 'open') set('open');
    e.stopPropagation();
  });
  document.getElementById('simScreen').addEventListener('click', () => { if (state === 'open') set(before); });

  // ---- Music: an original sample "song" at 112 bpm with a karaoke line.
  const lines = [
    'City lights are humming low',
    'We keep the windows open wide',
    'Every corner hums a song',
    'Hold the moment, let it glide',
    'Little island, glowing bright',
    'Carry me into the night'
  ];
  const lineSecs = 3.2;
  const bpm = 112;
  const palette = ['#ff6fa3', '#8a6cff', '#38d5ff'];
  const t0 = performance.now();

  function music(now) {
    const t = (now - t0) / 1000;
    const i = Math.floor(t / lineSecs) % lines.length;
    const prog = (t % lineSecs) / lineSecs;
    if (lineEl.dataset.i !== String(i)) {
      lineEl.dataset.i = String(i);
      lineEl.firstChild.nodeValue = lines[i];
      sungEl.textContent = lines[i];
    }
    sungEl.style.width = (prog * 100).toFixed(1) + '%';
    // Beat: a hit on every beat, a bigger one on the first of each bar, easing back down.
    const beat = (t * bpm / 60);
    const phase = beat % 1;
    const strong = Math.floor(beat) % 4 === 0 ? 1 : 0.6;
    const env = strong * Math.pow(1 - phase, 3);
    eq.forEach((s, k) => {
      const h = 4 + 10 * Math.abs(Math.sin(t * (5 + k * 1.7) + k)) * (0.5 + env * 0.6);
      s.style.height = h.toFixed(1) + 'px';
    });
    aurora(t, env);
  }

  // Three soft lights in the cover's colours drifting along the bottom and swelling on the beat.
  const ctx = canvas.getContext('2d');
  function aurora(t, env) {
    const dpr = window.devicePixelRatio || 1;
    const w = canvas.clientWidth, h = canvas.clientHeight;
    if (canvas.width !== Math.round(w * dpr)) { canvas.width = Math.round(w * dpr); canvas.height = Math.round(h * dpr); }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    const paths = [[0.22, 0.75, 0], [0.5, 0.55, 2.1], [0.78, 0.9, 4.2]];
    paths.forEach(([home, speed, phase], i) => {
      const x = w * (home + 0.17 * Math.sin(t * speed + phase));
      const rise = 0.8 + 0.2 * Math.sin(t * speed * 1.7 + phase * 1.3);
      const rx = w * (0.28 + 0.05 * Math.cos(t * speed * 0.8 + phase)) * (0.92 + 0.25 * env);
      const ry = h * (0.42 + 0.3 * env) * rise;
      const cy = h + h * 0.22;
      const g = ctx.createRadialGradient(0, 0, 0, 0, 0, 1);
      const c = palette[i % palette.length];
      const a = 0.26 + 0.22 * env;
      g.addColorStop(0, hexA(c, a));
      g.addColorStop(0.45, hexA(c, a * 0.55));
      g.addColorStop(1, hexA(c, 0));
      ctx.save();
      ctx.translate(x, cy);
      ctx.scale(rx, ry);
      ctx.fillStyle = g;
      ctx.beginPath();
      ctx.arc(0, 0, 1, 0, Math.PI * 2);
      ctx.fill();
      ctx.restore();
    });
  }
  function hexA(hex, a) {
    const n = parseInt(hex.slice(1), 16);
    return `rgba(${n >> 16},${(n >> 8) & 255},${n & 255},${a.toFixed(3)})`;
  }

  // ---- Panel: each tab grows out of its icon on a soft spring.
  let current = 0;
  let anim = null;
  function spring(t) {
    const w = Math.PI * 2 / 0.65, z = 0.78, wd = w * Math.sqrt(1 - z * z);
    return 1 - Math.exp(-z * w * t) * (Math.cos(wd * t) + z * w / wd * Math.sin(wd * t));
  }
  function openTab(i, fromBtn) {
    current = i;
    tabs.forEach((b, k) => b.classList.toggle('on', k === i));
    img.src = tabs[i].dataset.img;
    img.alt = tabs[i].getAttribute('aria-label') + ' tab';
    const btn = fromBtn || null;
    body.style.transform = "none";
    const br = body.getBoundingClientRect();
    let from;
    if (btn) {
      const r = btn.getBoundingClientRect();
      from = { x: r.left - br.left, y: r.top - br.top, w: r.width, h: r.height };
    } else {
      from = { x: br.width / 2 - 60, y: -20, w: 120, h: 30 }; // out of the notch
    }
    if (reduce) { body.style.transform = "none"; body.style.opacity = "1"; return; }
    const start = performance.now();
    anim = { start, from, w: br.width, h: br.height };
  }
  function panel(now) {
    if (!anim) return;
    const t = (now - anim.start) / 1000;
    const e = spring(t);
    const { from, w, h } = anim;
    const x = from.x + (0 - from.x) * e, y = from.y + (0 - from.y) * e;
    const sw = (from.w + (w - from.w) * e) / w, sh = (from.h + (h - from.h) * e) / h;
    body.style.transform = `translate(${x}px, ${y}px) scale(${sw}, ${sh})`;
    body.style.opacity = Math.min(1, e * 1.6).toFixed(3);
    if (t > 1) { body.style.transform = "none"; body.style.opacity = "1"; anim = null; }
  }
  tabs.forEach((b, i) => b.addEventListener('click', e => { e.stopPropagation(); openTab(i, b); }));

  function frame(now) {
    if (state === 'music') music(now);
    if (state === 'open') panel(now);
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
  set('idle');
})();
