/* Playerbots operator UI. Vanilla JS, no libraries, no network access beyond this origin.
 *
 * Data flow: poll /api/bots + /api/stats (1 s), /api/markers (5 s), /api/bots/:id (2 s while a bot
 * is selected), /api/map (30 s). Only while the tab is visible. Bot dots are interpolated between
 * polls; the canvas only redraws when something changed (requestAnimationFrame on demand).
 * The Issues view polls /api/issues (or /api/issues/:fp in the detail view) every 5 s, only while
 * it is on screen; the open/queued badge follows the 1 s /api/stats poll.
 * Every string that comes from the bot process is inserted with textContent, never innerHTML.
 */
(() => {
'use strict';

// ------------------------------------------------------------------ constants
const ACTIVITIES = ['offline', 'connecting', 'idle', 'traveling', 'questing', 'grinding', 'fighting',
  'dueling', 'trading', 'trade_run', 'housing', 'resting', 'dead'];
const ACT = {
  offline:    { label: 'Offline',    color: '#9aa0a6' },
  connecting: { label: 'Connecting', color: '#c9ccd2' },
  idle:       { label: 'Idle',       color: '#4f8ef7' },
  traveling:  { label: 'Traveling',  color: '#17b3c4' },
  questing:   { label: 'Questing',   color: '#f2b01e' },
  grinding:   { label: 'Grinding',   color: '#f2711c' },
  fighting:   { label: 'Fighting',   color: '#e5383b' },
  dueling:    { label: 'Dueling',    color: '#d62f8a' },
  trading:    { label: 'Trading',    color: '#2fb344' },
  trade_run:  { label: 'Trade run',  color: '#8bc34a' },
  housing:    { label: 'Housing',    color: '#a0693a' },
  resting:    { label: 'Resting',    color: '#9b6bdf' },
  dead:       { label: 'Dead',       color: '#111111' },
};
const ACT_INDEX = {};
ACTIVITIES.forEach((a, i) => { ACT_INDEX[a] = i; });
const ACT_COLORS = ACTIVITIES.map((a) => ACT[a].color);

const GOALS = [['auto', 'Auto'], ['quest', 'Quest'], ['grind', 'Grind'], ['duel', 'Duel'], ['trade', 'Trade'],
  ['trade_run', 'Trade run'], ['housing', 'Housing'], ['dungeon', 'Dungeon'], ['sail', 'Sail'],
  ['auction', 'Auction'], ['market', 'Market'], ['guild', 'Guild'], ['gank', 'Gank'], ['warzone', 'Warzone'],
  ['event', 'Event'], ['companion', 'Companion'], ['farm', 'Farm'], ['rest', 'Rest'], ['wander', 'Wander']];

const MARKER = {
  house:        { label: 'House',        color: '#a0693a', minScale: 0 },
  duel_flag:    { label: 'Duel flag',    color: '#d62f8a', minScale: 0 },
  quest_giver:  { label: 'Quest giver',  color: '#f2b01e', minScale: 0.12 },
  trade_outlet: { label: 'Trade outlet', color: '#2fb344', minScale: 0 },
};
const MARKER_KINDS = Object.keys(MARKER);

const POLL = { bots: 1000, markers: 5000, detail: 2000, map: 30000, issues: 5000 };
const FLASH_MS = 30000;   // how long the "show on map" marker of an issue stays
const ROW_H = 28;
const TAU = Math.PI * 2;
const MAX_SCALE = 80;
const NAME_SCALE = 1.0;   // px per metre at which every bot gets a name label
const NAME_MAX_BOTS = 30; // ...or when this few bots are on screen

// ------------------------------------------------------------------ helpers
const $ = (id) => document.getElementById(id);
const nf = new Intl.NumberFormat();
const coll = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' });

function h(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text;
  return e;
}
function setText(e, v) { if (e.textContent !== v) e.textContent = v; }
function num(v) { return typeof v === 'number' && isFinite(v) ? v : 0; }
function clamp(v, lo, hi) { return v < lo ? lo : v > hi ? hi : v; }
function lsGet(k) { try { return localStorage.getItem(k); } catch (_) { return null; } }
function lsSet(k, v) { try { localStorage.setItem(k, v); } catch (_) { /* private mode */ } }

function fmtBytes(n) {
  n = num(n);
  if (n < 1024) return nf.format(n) + ' B';
  const u = ['KB', 'MB', 'GB', 'TB'];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return (n >= 100 ? n.toFixed(0) : n.toFixed(1)) + ' ' + u[i];
}
function fmtCount(n) {
  n = num(n);
  if (n >= 1e9) return (n / 1e9).toFixed(1) + 'G';
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1e4) return (n / 1e3).toFixed(0) + 'k';
  return nf.format(Math.round(n));
}
function fmtDur(s) {
  s = Math.max(0, Math.floor(num(s)));
  const d = Math.floor(s / 86400), hh = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60);
  if (d) return d + 'd ' + hh + 'h';
  if (hh) return hh + 'h ' + m + 'm';
  if (m) return m + 'm ' + (s % 60) + 's';
  return s + 's';
}
function fmtClock(ms) {
  const d = new Date(ms);
  const p = (v) => String(v).padStart(2, '0');
  return p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds());
}
function fmtRate(v) { v = num(v); return v >= 100 ? v.toFixed(0) : v.toFixed(1); }

// ------------------------------------------------------------------ state
const S = {
  bots: new Map(), list: [], view: [], gen: 0,
  pollT: 0, animDur: 1000, animUntil: 0, lastBotsAt: 0, posDirty: false,
  stats: null, markers: [], counts: new Array(ACTIVITIES.length).fill(0),
  map: { info: null, img: null, key: '' },
  sel: null, detail: null, follow: false, showMarkers: true, showLines: true, showNames: true,
  filter: new Set(), search: '', sort: { key: 'name', dir: 1 },
  W: 0, H: 0, dpr: 1, cx: 0, cy: 0, scale: 0.02, userMoved: false, fitted: false, vanim: null,
  mouse: { x: 0, y: 0, in: false, touch: false }, hover: null, hoverMarker: null, dragging: false,
  sx: new Float32Array(0), sy: new Float32Array(0), on: new Uint8Array(0),
  failures: 0, lastOkAt: 0, token: '', partnerSig: '', restoredHash: false, flash: null,
};
const C = {}; // theme colours read from CSS variables

function readTheme() {
  const cs = getComputedStyle(document.documentElement);
  for (const k of ['map-bg', 'grid', 'grid-text', 'label', 'halo', 'accent', 'panel', 'border', 'muted', 'danger']) {
    C[k] = cs.getPropertyValue('--' + k).trim() || '#888';
  }
}

// ------------------------------------------------------------------ bots
function applySnapshots(arr, now) {
  const oldDur = S.animDur, oldT = S.pollT;
  const p = clamp((now - oldT) / oldDur, 0, 1);
  S.gen++;
  S.list.length = 0;
  for (const s of arr) {
    const x = num(s.x), y = num(s.y);
    let b = S.bots.get(s.id);
    if (!b) {
      b = { id: s.id, x, y, dx: x, dy: y, fx: x, fy: y, tx: x, ty: y, vis: true, gen: 0 };
      S.bots.set(s.id, b);
    } else {
      // continue from where the dot is drawn right now
      const cx = b.fx + (b.tx - b.fx) * p, cy = b.fy + (b.ty - b.fy) * p;
      const jumped = (b.tx === 0 && b.ty === 0) || Math.hypot(x - b.tx, y - b.ty) > 150;
      b.fx = jumped ? x : cx;
      b.fy = jumped ? y : cy;
      b.tx = x; b.ty = y; b.x = x; b.y = y;
    }
    b.gen = S.gen;
    b.name = s.name || ('#' + s.id);
    b.race = s.race || '';
    b.cls = s.class || '';
    b.level = s.level | 0;
    b.hp = s.hp_pct | 0;
    b.z = num(s.z);
    b.zone = s.zone || '';
    b.activity = ACT_INDEX[s.activity] === undefined ? 'idle' : s.activity;
    b.ai = ACT_INDEX[b.activity];
    b.goal = s.goal || '';
    b.detail = s.detail || '';
    b.partner = s.partner == null ? null : s.partner;
    b.gold = num(s.gold);
    b.house = !!s.has_house;
    b.online = s.online_secs | 0;
    b.paused = !!s.paused;
    b.key = (b.name + ' #' + b.id + ' ' + b.race + ' ' + b.cls + ' ' + b.zone + ' ' + b.goal + ' ' +
      ACT[b.activity].label + ' ' + b.detail).toLowerCase();
    S.list.push(b);
  }
  if (S.bots.size !== S.list.length) {
    for (const [id, b] of S.bots) { if (b.gen !== S.gen) S.bots.delete(id); }
  }
  S.animDur = clamp(S.lastBotsAt ? now - S.lastBotsAt : 1000, 500, 2500);
  S.lastBotsAt = now;
  S.pollT = now;
  S.animUntil = now + S.animDur;
  S.posDirty = true;
}

const SORTERS = {
  name: (a, b) => coll.compare(a.name, b.name),
  level: (a, b) => a.level - b.level,
  class: (a, b) => coll.compare(a.cls, b.cls),
  activity: (a, b) => a.ai - b.ai,
  zone: (a, b) => (a.zone === '') - (b.zone === '') || coll.compare(a.zone, b.zone),
};

function refreshView() {
  const q = S.search.trim().toLowerCase();
  const f = S.filter;
  const out = S.view;
  out.length = 0;
  S.counts.fill(0);
  for (const b of S.list) {
    const hit = !q || b.key.includes(q);
    if (hit) S.counts[b.ai]++;
    b.vis = hit && (f.size === 0 || f.has(b.activity));
    if (b.vis) out.push(b);
  }
  const cmp = SORTERS[S.sort.key] || SORTERS.name;
  const dir = S.sort.dir;
  out.sort((a, b) => (cmp(a, b) || a.id - b.id) * dir);
  renderChips();
  renderList();
  setText($('list-foot'), out.length === S.list.length
    ? nf.format(S.list.length) + ' bots'
    : nf.format(out.length) + ' of ' + nf.format(S.list.length) + ' bots');
  $('empty').hidden = S.list.length > 0;
  requestDraw();
}

// ------------------------------------------------------------------ chips, legend, stats
const chipEls = {};
let chipAll = null;

function buildChips() {
  const wrap = $('chips');
  chipAll = h('button', 'chip');
  chipAll.type = 'button';
  chipAll.append(h('span', null, 'All'), h('b', null, '0'));
  chipAll.addEventListener('click', () => { S.filter.clear(); refreshView(); });
  wrap.append(chipAll);
  for (const a of ACTIVITIES) {
    const b = h('button', 'chip');
    b.type = 'button';
    const dot = h('i');
    dot.style.background = ACT[a].color;
    const cnt = h('b', null, '0');
    b.append(dot, h('span', null, ACT[a].label), cnt);
    b.addEventListener('click', () => {
      if (S.filter.has(a)) S.filter.delete(a); else S.filter.add(a);
      refreshView();
    });
    wrap.append(b);
    chipEls[a] = { b, cnt };
  }
}

function renderChips() {
  chipAll.setAttribute('aria-pressed', S.filter.size === 0 ? 'true' : 'false');
  let total = 0;
  for (const a of ACTIVITIES) {
    const n = S.counts[ACT_INDEX[a]];
    total += n;
    const c = chipEls[a];
    setText(c.cnt, String(n));
    c.b.setAttribute('aria-pressed', S.filter.has(a) ? 'true' : 'false');
    c.b.classList.toggle('empty', n === 0 && !S.filter.has(a));
  }
  setText(chipAll.lastChild, String(total));
}

function buildLegend() {
  const lg = $('legend');
  for (const a of ACTIVITIES) {
    const s = h('span');
    const i = h('i');
    i.style.background = ACT[a].color;
    s.append(i, document.createTextNode(ACT[a].label));
    lg.append(s);
  }
  const shapes = { house: 'sq', duel_flag: '', quest_giver: '', trade_outlet: 'dia' };
  for (const k of MARKER_KINDS) {
    const s = h('span');
    const i = h('i', shapes[k]);
    i.style.background = MARKER[k].color;
    s.append(i, document.createTextNode(MARKER[k].label));
    lg.append(s);
  }
}

function setTile(id, main, sub) {
  const b = $(id);
  const key = main + '|' + (sub || '');
  if (b._v === key) return;
  b._v = key;
  b.textContent = main;
  if (sub) b.append(h('small', null, sub));
}

function renderStats() {
  const s = S.stats;
  if (!s) return;
  setTile('st-online', nf.format(s.bots_online) + ' / ' + nf.format(s.bots_total), null);
  setTile('st-mem', fmtBytes(s.rss_bytes), fmtBytes(s.bytes_per_bot) + '/bot');
  setTile('st-pkt', fmtRate(s.packets_in_per_sec) + ' in', fmtRate(s.packets_out_per_sec) + ' out');
  setTile('st-bw', fmtBytes(s.bytes_in_per_sec) + '/s in', fmtBytes(s.bytes_out_per_sec) + '/s out');
  setTile('st-up', fmtDur(s.uptime_secs), null);
  const l = s.llm;
  $('st-llm-tile').hidden = !l;
  if (l) {
    const unlimited = !(num(l.budget_left) < 1e15);
    const extra = [];
    if (num(l.rate_limited)) extra.push(nf.format(l.rate_limited) + ' throttled');
    if (num(l.chat_replies)) extra.push(nf.format(l.chat_replies) + ' chat replies');
    if (num(l.director_picks)) extra.push(nf.format(l.director_picks) + ' director picks');
    setTile('st-llm', nf.format(l.requests) + ' requests, ' + nf.format(l.failures) + ' failed',
      fmtCount(l.tokens_today) + ' tokens today, ' + (unlimited ? 'no budget limit' : fmtCount(l.budget_left) + ' left') +
      (extra.length ? ' \u00b7 ' + extra.join(', ') : ''));
  }
  document.title = 'Playerbots (' + s.bots_online + '/' + s.bots_total + ')';
  renderIssueBadge(s);
}

function renderConn() {
  const c = $('conn');
  const age = S.lastOkAt ? (performance.now() - S.lastOkAt) / 1000 : Infinity;
  let cls = 'conn', text;
  if (S.failures >= 2 || age > 6) { cls += ' down'; text = S.lastOkAt ? 'no data for ' + Math.floor(age) + ' s' : 'cannot reach the bot process'; }
  else if (S.lastOkAt) { cls += ' live'; text = 'live'; }
  else text = 'connecting';
  if (c.className !== cls) c.className = cls;
  setText($('conn-text'), text);
}

// ------------------------------------------------------------------ list (virtualised)
const L = { el: null, spacer: null, rows: [] };

function createRow() {
  const r = h('div', 'row');
  r.setAttribute('role', 'option');
  const name = h('span', 'c-name');
  const dot = h('i');
  const em = h('em');
  name.append(dot, em);
  const lvl = h('span', 'c-lvl'), cls = h('span', 'c-class'), act = h('span', 'c-act'), zone = h('span', 'c-zone');
  r.append(name, lvl, cls, act, zone);
  r._f = { dot, em, lvl, cls, act, zone };
  r._id = -1;
  L.spacer.append(r);
  return r;
}

function renderList() {
  const n = S.view.length;
  const hgt = n * ROW_H + 'px';
  if (L.spacer.style.height !== hgt) L.spacer.style.height = hgt;
  const need = Math.ceil(L.el.clientHeight / ROW_H) + 4;
  while (L.rows.length < need) L.rows.push(createRow());
  const first = Math.max(0, Math.floor(L.el.scrollTop / ROW_H) - 1);
  for (let k = 0; k < L.rows.length; k++) {
    const r = L.rows[k];
    const i = first + k;
    if (i >= n) {
      if (r._id !== -1) { r.style.display = 'none'; r._id = -1; }
      continue;
    }
    const b = S.view[i];
    if (r._id === -1) r.style.display = '';
    r._id = b.id;
    r.style.transform = 'translateY(' + i * ROW_H + 'px)';
    const f = r._f;
    setText(f.em, b.name);
    const col = ACT_COLORS[b.ai];
    if (f.dot._c !== col) { f.dot.style.background = col; f.dot._c = col; }
    setText(f.lvl, String(b.level));
    setText(f.cls, b.cls);
    setText(f.act, ACT[b.activity].label);
    setText(f.zone, b.zone);
    r.classList.toggle('sel', b.id === S.sel);
    r.classList.toggle('paused', b.paused);
    r.setAttribute('aria-selected', b.id === S.sel ? 'true' : 'false');
  }
}

function revealInList(id) {
  const i = S.view.findIndex((b) => b.id === id);
  if (i < 0) return;
  const top = i * ROW_H, h = L.el.clientHeight;
  if (top < L.el.scrollTop) L.el.scrollTop = top;
  else if (top + ROW_H > L.el.scrollTop + h) L.el.scrollTop = top + ROW_H - h;
  renderList();
}

function buildList() {
  L.el = $('list');
  L.spacer = $('list-spacer');
  let pending = false;
  L.el.addEventListener('scroll', () => {
    if (pending) return;
    pending = true;
    requestAnimationFrame(() => { pending = false; renderList(); });
  }, { passive: true });
  L.el.addEventListener('click', (e) => {
    const r = e.target.closest('.row');
    if (r && r._id >= 0) select(r._id, { center: true });
  });
  L.el.addEventListener('keydown', (e) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp' && e.key !== 'Enter') return;
    e.preventDefault();
    const i = S.view.findIndex((b) => b.id === S.sel);
    if (e.key === 'Enter') { if (i >= 0) select(S.sel, { center: true }); return; }
    const j = clamp(i < 0 ? 0 : i + (e.key === 'ArrowDown' ? 1 : -1), 0, S.view.length - 1);
    if (S.view[j]) select(S.view[j].id, { center: true, reveal: true });
  });
  new ResizeObserver(renderList).observe(L.el);
  for (const btn of document.querySelectorAll('#list-head button')) {
    btn.addEventListener('click', () => {
      const key = btn.dataset.sort;
      S.sort = S.sort.key === key ? { key, dir: -S.sort.dir } : { key, dir: 1 };
      lsSet('pb_sort', JSON.stringify(S.sort));
      markSort();
      refreshView();
    });
  }
  markSort();
}

function markSort() {
  for (const btn of document.querySelectorAll('#list-head button')) {
    const on = btn.dataset.sort === S.sort.key;
    btn.classList.toggle('sorted', on);
    btn.classList.toggle('desc', on && S.sort.dir < 0);
  }
}

// ------------------------------------------------------------------ selection and detail panel
const D = {};
const FIELD_DEFS = [['goal', 'Goal'], ['detail', 'Detail'], ['activity', 'Activity'], ['zone', 'Zone'],
  ['pos', 'Position'], ['gold', 'Gold'], ['house', 'House'], ['online', 'Online for'],
  ['partner', 'Partner'], ['state', 'State'], ['id', 'Bot id']];

function buildDetail() {
  D.panel = $('detail');
  D.name = $('d-name'); D.sub = $('d-sub'); D.hp = $('d-hp'); D.hpText = $('d-hp-text');
  D.quests = $('d-quests'); D.extra = $('d-extra'); D.extraWrap = $('d-extra-wrap'); D.log = $('d-log');
  D.chat = $('d-chat'); D.chatWrap = $('d-chat-wrap');
  D.status = $('c-status'); D.pause = $('c-pause'); D.goal = $('c-goal'); D.duel = $('c-duel');
  D.trade = $('c-trade'); D.say = $('c-say'); D.whisper = $('c-whisper'); D.whisperTo = $('c-whisper-to');
  D.f = {};
  for (const [key, label] of FIELD_DEFS) {
    const dd = h('dd');
    $('d-fields').append(h('dt', null, label), dd);
    D.f[key] = dd;
  }
  for (const [value, label] of GOALS) {
    const o = h('option', null, label);
    o.value = value;
    D.goal.append(o);
  }
  $('d-close').addEventListener('click', () => select(null));

  D.pause.addEventListener('click', () => {
    const b = S.bots.get(S.sel);
    if (b) sendCommand(S.sel, { type: b.paused ? 'resume' : 'pause' }, D.status);
  });
  $('c-goal-set').addEventListener('click', () => {
    if (S.sel != null) sendCommand(S.sel, { type: 'goal', goal: D.goal.value }, D.status);
  });
  $('c-duel-go').addEventListener('click', () => {
    if (S.sel != null && D.duel.value) sendCommand(S.sel, { type: 'duel_with', other: Number(D.duel.value) }, D.status);
    else setStatus(D.status, 'err', 'Pick another bot first.');
  });
  $('c-trade-go').addEventListener('click', () => {
    if (S.sel != null && D.trade.value) sendCommand(S.sel, { type: 'trade_with', other: Number(D.trade.value) }, D.status);
    else setStatus(D.status, 'err', 'Pick another bot first.');
  });
  $('c-say-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const text = D.say.value.trim();
    if (S.sel == null) return;
    if (!text) { setStatus(D.status, 'err', 'Type something to say.'); return; }
    if (await sendCommand(S.sel, { type: 'say', text }, D.status)) D.say.value = '';
  });
  $('c-whisper-form').addEventListener('submit', async (e) => {
    e.preventDefault();
    const to = D.whisperTo.value.trim();
    const text = D.whisper.value.trim();
    if (S.sel == null) return;
    if (!to || !text) { setStatus(D.status, 'err', 'Give a character name and a line to whisper.'); return; }
    if (await sendCommand(S.sel, { type: 'whisper', to, text }, D.status)) D.whisper.value = '';
  });
}

function select(id, opts) {
  opts = opts || {};
  const changed = S.sel !== id;
  S.sel = id;
  if (changed) {
    S.detail = null;
    setStatus(D.status, '', '');
    D.say.value = '';
    D.whisper.value = '';
    S.partnerSig = '';
    try { history.replaceState(null, '', id == null ? location.pathname + location.search : '#bot=' + id); } catch (_) { /* ignore */ }
  }
  D.panel.hidden = id == null;
  $('sidebar').classList.toggle('has-detail', id != null);
  if (id != null) {
    const b = S.bots.get(id);
    if (changed && b) {
      const g = GOALS.find(([v]) => v === b.goal);
      D.goal.value = g ? g[0] : 'auto';
    }
    if (opts.openPanel) { setPanel(true); if (I.tab) setTab(false); }
    renderDetail();
    if (opts.openPanel && changed) revealDetail();
    if (changed) { last.detail = -Infinity; tick(); }
  }
  if (opts.center && id != null) {
    const b = S.bots.get(id);
    if (b) centerOn(b);
  }
  if (opts.reveal && id != null) revealInList(id);
  renderList();
  requestDraw();
}

// Selecting on the map: scroll the side panel so the detail card is in view.
function revealDetail() {
  const sb = $('sidebar');
  requestAnimationFrame(() => {
    if (sb.scrollHeight <= sb.clientHeight) return;
    const top = D.panel.getBoundingClientRect().top - sb.getBoundingClientRect().top + sb.scrollTop - 10;
    sb.scrollTo({ top, behavior: 'smooth' });
  });
}

function fillPartnerSelects() {
  let sig = S.sel + ':' + S.list.length;
  for (const b of S.list) sig += ',' + b.id + (b.ai > 1 ? '' : 'x');
  if (sig === S.partnerSig) return;
  if (document.activeElement === D.duel || document.activeElement === D.trade) return;
  S.partnerSig = sig;
  const others = S.list.filter((b) => b.id !== S.sel && b.ai > 1).sort((a, b) => coll.compare(a.name, b.name));
  for (const [sel, label] of [[D.duel, 'Duel with…'], [D.trade, 'Trade with…']]) {
    const keep = sel.value;
    const frag = document.createDocumentFragment();
    const first = h('option', null, label);
    first.value = '';
    frag.append(first);
    for (const b of others) {
      const o = h('option', null, b.name + ' (Lv ' + b.level + ' ' + b.cls + ')');
      o.value = String(b.id);
      frag.append(o);
    }
    sel.replaceChildren(frag);
    if (keep && others.some((b) => String(b.id) === keep)) sel.value = keep;
  }
}

function renderDetail() {
  if (S.sel == null) return;
  const live = S.bots.get(S.sel);
  const d = S.detail && S.detail.snapshot && S.detail.snapshot.id === S.sel ? S.detail : null;
  // The 1 s list snapshot is fresher than the 2 s detail; fall back to the detail when the list
  // does not have the bot (yet).
  const b = live || (d && snapshotToView(d.snapshot));
  if (!b) {
    setText(D.name, 'Bot #' + S.sel);
    setText(D.sub, 'waiting for data…');
    return;
  }
  setText(D.name, b.name);
  setText(D.sub, 'Lv ' + b.level + ' ' + [b.race, b.cls].filter(Boolean).join(' ') + ' · ' + ACT[b.activity].label);
  D.hp.style.width = clamp(b.hp, 0, 100) + '%';
  const hpCls = b.hp < 25 ? 'low' : b.hp < 60 ? 'mid' : '';
  if (D.hp.className !== hpCls) D.hp.className = hpCls;
  setText(D.hpText, b.hp + '%');
  const f = D.f;
  setText(f.goal, b.goal || '-');
  setText(f.detail, b.detail || '-');
  setText(f.activity, ACT[b.activity].label);
  setText(f.zone, b.zone || '-');
  setText(f.pos, b.x.toFixed(1) + ', ' + b.y.toFixed(1) + ', ' + b.z.toFixed(1));
  setText(f.gold, nf.format(b.gold));
  setText(f.house, b.house ? 'owns a house' : 'none');
  setText(f.online, fmtDur(b.online));
  setText(f.state, b.paused ? 'paused' : 'running');
  setText(f.id, String(b.id));
  setText(D.pause, b.paused ? 'Resume' : 'Pause');
  renderPartner(b);
  fillPartnerSelects();

  if (!d) return;
  // quests
  const qs = d.quests || [];
  const qf = document.createDocumentFragment();
  if (!qs.length) qf.append(h('li', 'empty', 'No quests.'));
  for (const q of qs) {
    const li = h('li');
    li.append(h('span', null, (q.name || 'Quest') + ' #' + q.id), h('span', null, q.status || ''));
    qf.append(li);
  }
  D.quests.replaceChildren(qf);
  // extra rows
  const ex = d.extra || [];
  D.extraWrap.hidden = ex.length === 0;
  const ef = document.createDocumentFragment();
  for (const kv of ex) ef.append(h('dt', null, String(kv[0])), h('dd', null, String(kv[1])));
  D.extra.replaceChildren(ef);
  // chat the bot heard and said, newest first
  const chat = (d.chat || []).slice().sort((a, b) => b.t - a.t).slice(0, 8);
  D.chatWrap.hidden = chat.length === 0;
  const cf = document.createDocumentFragment();
  for (const ln of chat) {
    const li = h('li');
    li.append(h('time', null, fmtClock(ln.t)), h('span', null, ln.text));
    cf.append(li);
  }
  D.chat.replaceChildren(cf);
  // log, newest first
  const log = (d.log || []).slice().sort((a, b) => b.t - a.t).slice(0, 80);
  const lf = document.createDocumentFragment();
  if (!log.length) lf.append(h('li', 'empty', 'Nothing logged yet.'));
  for (const ln of log) {
    const li = h('li');
    const t = h('time', null, fmtClock(ln.t));
    li.append(t, h('span', null, ln.text));
    lf.append(li);
  }
  D.log.replaceChildren(lf);
}

function snapshotToView(s) {
  return {
    id: s.id, name: s.name || ('#' + s.id), race: s.race || '', cls: s.class || '', level: s.level | 0, hp: s.hp_pct | 0,
    x: num(s.x), y: num(s.y), z: num(s.z), zone: s.zone || '', activity: ACT_INDEX[s.activity] === undefined ? 'idle' : s.activity,
    goal: s.goal || '', detail: s.detail || '', partner: s.partner == null ? null : s.partner, gold: num(s.gold),
    house: !!s.has_house, online: s.online_secs | 0, paused: !!s.paused,
  };
}

function renderPartner(b) {
  const dd = D.f.partner;
  if (b.partner == null) { setText(dd, '-'); dd._p = null; return; }
  const p = S.bots.get(b.partner);
  const label = p ? p.name : '#' + b.partner;
  if (dd._p !== b.partner) {
    dd._p = b.partner;
    const btn = h('button', 'link');
    btn.type = 'button';
    btn.addEventListener('click', () => select(dd._p, { center: true }));
    dd.replaceChildren(btn);
  }
  setText(dd.firstChild, label);
}

// ------------------------------------------------------------------ commands
const statusTimers = new WeakMap();
function setStatus(el, kind, text) {
  el.className = 'status' + (kind ? ' ' + kind : '');
  el.textContent = text;
  clearTimeout(statusTimers.get(el));
  if (kind === 'ok') statusTimers.set(el, setTimeout(() => { if (el.textContent === text) { el.textContent = ''; el.className = 'status'; } }, 5000));
}

// POST a JSON body with the operator token (if any). Resolves to { ok: true } or { ok: false, error }.
async function postJson(url, payload) {
  const headers = { 'content-type': 'application/json' };
  if (S.token) headers['x-playerbots-token'] = S.token;
  try {
    const ac = new AbortController();
    const t = setTimeout(() => ac.abort(), 8000);
    let r;
    try {
      r = await fetch(url, { method: 'POST', headers, body: JSON.stringify(payload), signal: ac.signal });
    } finally { clearTimeout(t); }
    let body = null;
    try { body = await r.json(); } catch (_) { /* no JSON body */ }
    if (r.ok) return { ok: true };
    let msg = (body && body.error) || ('HTTP ' + r.status);
    if (r.status === 401) {
      msg = 'Not authorised: enter the API token under Global controls. (' + msg + ')';
      $('global').open = true;
      $('g-token').focus();
    }
    return { ok: false, error: msg };
  } catch (e) {
    return { ok: false, error: 'Request failed: ' + (e && e.name === 'AbortError' ? 'timed out' : (e && e.message) || e) };
  }
}

async function sendCommand(bot, cmd, statusEl) {
  const url = bot == null ? '/api/command' : '/api/bots/' + bot + '/command';
  setStatus(statusEl, '', 'Sending ' + cmd.type + '…');
  const r = await postJson(url, cmd);
  if (r.ok) {
    setStatus(statusEl, 'ok', 'Sent: ' + describe(cmd));
    last.bots = -Infinity; last.detail = -Infinity; tick();
    return true;
  }
  setStatus(statusEl, 'err', r.error);
  return false;
}

function describe(cmd) {
  switch (cmd.type) {
    case 'goal': return 'goal ' + cmd.goal;
    case 'duel_with': case 'trade_with': return cmd.type.replace('_with', '') + ' with #' + cmd.other;
    case 'say': return 'say';
    case 'whisper': return 'whisper to ' + cmd.to;
    case 'spawn': case 'despawn': return cmd.type + ' ' + cmd.count;
    default: return cmd.type;
  }
}

function buildGlobal() {
  const st = $('g-status');
  S.token = lsGet('pb_token') || '';
  $('g-token').value = S.token;
  $('g-token').addEventListener('input', (e) => { S.token = e.target.value.trim(); lsSet('pb_token', S.token); });
  const count = () => {
    const n = Math.floor(Number($('g-count').value));
    return n >= 1 && n <= 1000 ? n : 0;
  };
  for (const [id, type] of [['g-spawn', 'spawn'], ['g-despawn', 'despawn']]) {
    $(id).addEventListener('click', () => {
      const n = count();
      if (!n) { setStatus(st, 'err', 'Enter a count between 1 and 1000.'); return; }
      sendCommand(null, { type, count: n }, st);
    });
  }
  $('g-pause').addEventListener('click', () => sendCommand(null, { type: 'pause' }, st));
  $('g-resume').addEventListener('click', () => sendCommand(null, { type: 'resume' }, st));
}

// ------------------------------------------------------------------ issues
// Problems of the game server that the bots ran into, deduplicated by the bot process.
const ISSUE_STATES = ['open', 'queued', 'ignored', 'resolved'];
const STATE_LABEL = { open: 'Open', queued: 'Queued', ignored: 'Ignored', resolved: 'Resolved' };
const STATE_CLS = { open: 'st-open', queued: 'st-queued', ignored: 'st-ignored', resolved: 'st-resolved' };
const SEV_CLS = { high: 'sev-high', medium: 'sev-medium', low: 'sev-low' };
// What the operator can do in each state (the server refuses the rest, e.g. queueing twice).
const ISSUE_ACTIONS = {
  open: ['queue', 'ignore', 'resolve'], queued: ['ignore', 'resolve'],
  ignored: ['reopen', 'resolve'], resolved: ['reopen', 'ignore'],
};
const ACTION_INFO = {
  queue: ['Queue', 'Write the entry to the team issue queue now'],
  ignore: ['Ignore', 'Never queue this one by itself'],
  resolve: ['Resolve', 'Mark it as fixed'],
  reopen: ['Reopen', 'Back to open'],
};
const ISSUE_MAX_ROWS = 300;
const POS_RE = /\bat \((-?\d+(?:\.\d+)?), (-?\d+(?:\.\d+)?), (-?\d+(?:\.\d+)?)\)/g;

const I = {
  tab: false, list: [], loaded: false, states: new Set(['open', 'queued']),
  sel: null, detail: null, sampleKey: '', initFor: '', open: new Set(), acting: false, badgeKey: '',
};
const IE = { rows: new Map(), chipEls: {} }; // elements

function words(s) { return String(s || '').replace(/_/g, ' '); }
function fmtAgo(ms) {
  if (!num(ms)) return '-';
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  return s < 5 ? 'just now' : fmtDur(s) + ' ago';
}
function fmtStamp(ms) { return num(ms) ? new Date(ms).toLocaleString() : '-'; }

function pill(text, classes) { return h('span', 'pill ' + (classes[text] || 'other'), text || '-'); }

function cleanSummary(s) {
  return {
    fingerprint: String(s.fingerprint || ''), kind: String(s.kind || ''), title: String(s.title || ''),
    suspect: String(s.suspect || ''), severity: String(s.severity || ''), count: num(s.count), bots: num(s.bots),
    first_seen: num(s.first_seen), last_seen: num(s.last_seen), state: String(s.state || ''), reportable: !!s.reportable,
  };
}

// The bot's position is only in the context line: "... at (x, y, z) in Zone". The last match wins.
function samplePos(s) {
  let m = null;
  for (const x of String(s.context || '').matchAll(POS_RE)) m = x;
  if (!m) return null;
  const x = Number(m[1]), y = Number(m[2]);
  return isFinite(x) && isFinite(y) ? { x, y, z: Number(m[3]) } : null;
}

function fillKv(dl, rows) {
  const f = document.createDocumentFragment();
  for (const [label, val, cls] of rows) {
    const dd = h('dd', cls || null);
    if (val instanceof Node) dd.append(val); else dd.textContent = String(val);
    f.append(h('dt', null, label), dd);
  }
  dl.replaceChildren(f);
}

function renderIssueBadge(s) {
  const open = num(s.open_issues) | 0, queued = num(s.queued_issues) | 0;
  const key = open + '/' + queued;
  if (key === I.badgeKey) return;
  I.badgeKey = key;
  const a = $('tab-open'), b = $('tab-queued');
  a.hidden = open === 0;
  b.hidden = queued === 0;
  setText(a, String(open));
  setText(b, String(queued));
  a.title = open + ' open';
  b.title = queued + ' queued';
  $('tab-issues').setAttribute('aria-label', 'Server issues: ' + open + ' open, ' + queued + ' queued');
}

function issuesVisible() { return I.tab && !document.body.classList.contains('panel-hidden'); }

// The operator is selecting text in `el` (a path to copy, a log line): do not rebuild it under them.
function selecting(el) {
  const sel = window.getSelection();
  return !!sel && !sel.isCollapsed && el.contains(sel.anchorNode);
}

function setTab(issues) {
  I.tab = issues;
  $('sidebar').classList.toggle('view-issues', issues);
  $('tab-bots').setAttribute('aria-pressed', issues ? 'false' : 'true');
  $('tab-issues').setAttribute('aria-pressed', issues ? 'true' : 'false');
  lsSet('pb_tab', issues ? 'issues' : 'bots');
  last.issues = -Infinity; // the 200 ms tick picks it up
}

function toggleIssueState(st) {
  if (st == null) I.states.clear();
  else if (I.states.has(st)) I.states.delete(st);
  else I.states.add(st);
  lsSet('pb_issue_states', JSON.stringify([...I.states]));
  renderIssueList();
}

function buildIssues() {
  for (const id of ['issues', 'i-listview', 'i-detailview', 'i-chips', 'i-status', 'i-list', 'i-foot', 'i-back', 'i-fp',
    'i-title', 'i-pills', 'i-actions', 'i-astatus', 'i-derr', 'i-fields', 'i-samples', 'i-entry']) {
    IE[id.replace(/^i-/, '')] = $(id);
  }
  try {
    const a = JSON.parse(lsGet('pb_issue_states') || 'null');
    if (Array.isArray(a)) I.states = new Set(a.filter((s) => ISSUE_STATES.includes(s)));
  } catch (_) { /* default filter */ }

  IE.chipAll = h('button', 'chip');
  IE.chipAll.type = 'button';
  IE.chipAll.append(h('span', null, 'All'), h('b', null, '0'));
  IE.chipAll.addEventListener('click', () => toggleIssueState(null));
  IE.chips.append(IE.chipAll);
  for (const st of ISSUE_STATES) {
    const b = h('button', 'chip');
    b.type = 'button';
    const cnt = h('b', null, '0');
    b.append(h('span', null, STATE_LABEL[st]), cnt);
    b.addEventListener('click', () => toggleIssueState(st));
    IE.chips.append(b);
    IE.chipEls[st] = { b, cnt };
  }

  $('tab-bots').addEventListener('click', () => setTab(false));
  $('tab-issues').addEventListener('click', () => setTab(true));
  IE.back.addEventListener('click', closeIssue);
  setTab(lsGet('pb_tab') === 'issues');
}

function renderIssueList() {
  const counts = {};
  for (const st of ISSUE_STATES) counts[st] = 0;
  for (const it of I.list) if (counts[it.state] !== undefined) counts[it.state]++;
  IE.chipAll.setAttribute('aria-pressed', I.states.size === 0 ? 'true' : 'false');
  setText(IE.chipAll.lastChild, String(I.list.length));
  for (const st of ISSUE_STATES) {
    const c = IE.chipEls[st];
    setText(c.cnt, String(counts[st]));
    c.b.setAttribute('aria-pressed', I.states.has(st) ? 'true' : 'false');
    c.b.classList.toggle('empty', counts[st] === 0 && !I.states.has(st));
  }

  const shown = I.list.filter((it) => I.states.size === 0 || I.states.has(it.state))
    .sort((a, b) => b.last_seen - a.last_seen || coll.compare(a.fingerprint, b.fingerprint));
  const focused = document.activeElement && IE.list.contains(document.activeElement) ? document.activeElement._fp : null;
  IE.rows.clear();
  const f = document.createDocumentFragment();
  for (const it of shown.slice(0, ISSUE_MAX_ROWS)) {
    const li = h('li');
    const b = h('button', 'issue-row');
    b.type = 'button';
    b._fp = it.fingerprint;
    b.title = it.fingerprint + ' · last seen ' + fmtStamp(it.last_seen);
    const top = h('span', 'i-top');
    top.append(pill(it.state, STATE_CLS), pill(it.severity, SEV_CLS), h('span', 'i-ago', fmtAgo(it.last_seen)));
    const meta = [words(it.kind), nf.format(it.count) + '×', nf.format(it.bots) + (it.bots === 1 ? ' bot' : ' bots'),
      'suspect ' + (it.suspect || 'unknown')];
    b.append(top, h('span', 'i-title', it.title || words(it.kind) || it.fingerprint), h('span', 'i-meta', meta.join(' · ')));
    b.addEventListener('click', () => openIssue(it.fingerprint));
    li.append(b);
    f.append(li);
    IE.rows.set(it.fingerprint, b);
  }
  if (!shown.length) {
    f.append(h('li', 'empty', !I.loaded ? 'Loading…' : I.list.length ? 'No issues in the selected states.' : 'No server issues reported.'));
  }
  IE.list.replaceChildren(f);
  if (focused && IE.rows.has(focused)) IE.rows.get(focused).focus({ preventScroll: true });
  setText(IE.foot, shown.length === I.list.length
    ? nf.format(I.list.length) + (I.list.length === 1 ? ' issue' : ' issues')
    : nf.format(shown.length) + ' of ' + nf.format(I.list.length) + ' issues' + (shown.length > ISSUE_MAX_ROWS ? ' (first ' + ISSUE_MAX_ROWS + ' shown)' : ''));
}

function showIssueView() {
  IE.listview.hidden = I.sel != null;
  IE.detailview.hidden = I.sel == null;
  IE.issues.classList.toggle('has-issue', I.sel != null);
}

function openIssue(fp) {
  I.sel = fp;
  I.detail = null;
  I.sampleKey = '';
  setStatus(IE.astatus, '', '');
  setText(IE.derr, '');
  showIssueView();
  const it = I.list.find((x) => x.fingerprint === fp);
  if (it) renderIssueDetail({ summary: it, samples: [], entry: '', area: null, evidence: null, queued_at: null, partial: true });
  IE.back.focus();
  last.issues = -Infinity;
  tick();
}

function closeIssue() {
  const fp = I.sel;
  I.sel = null;
  I.detail = null;
  showIssueView();
  renderIssueList();
  if (fp && IE.rows.has(fp)) IE.rows.get(fp).focus();
  last.issues = -Infinity;
  tick();
}

function renderIssueDetail(d) {
  const s = d.summary;
  const fp = s.fingerprint;
  I.detail = d;
  setText(IE.fp, fp);
  setText(IE.title, s.title || words(s.kind) || fp);
  IE.pills.replaceChildren(pill(s.state, STATE_CLS), pill(s.severity, SEV_CLS));
  renderIssueActions();

  const newest = (d.samples || []).reduce((a, x) => (!a || x.at > a.at ? x : a), null);
  let queue;
  if (s.state === 'queued') {
    queue = (d.queued_at ? 'queued ' + fmtAgo(d.queued_at) + ' (' + fmtStamp(d.queued_at) + ')' : 'queued') + '; marker ' + fp;
  } else if (s.state === 'open') {
    queue = s.reportable ? 'not queued yet; the policy queues it when its rules allow'
      : 'not queued; the policy never queues it by itself (Queue does it anyway)';
  } else {
    queue = 'not queued';
  }
  fillKv(IE.fields, [
    ['Expected', newest ? newest.expected || '-' : '-'],
    ['Actual', newest ? newest.actual || '-' : '-'],
    ['Kind', words(s.kind) || '-'],
    ['Suspect', s.suspect || 'unknown'],
    ['Seen', nf.format(s.count) + (s.count === 1 ? ' time by ' : ' times by ') + nf.format(s.bots) + (s.bots === 1 ? ' bot' : ' bots')],
    ['First seen', fmtAgo(s.first_seen) + ' (' + fmtStamp(s.first_seen) + ')'],
    ['Last seen', fmtAgo(s.last_seen) + ' (' + fmtStamp(s.last_seen) + ')'],
    ['Queue', queue],
    ['Evidence', d.evidence || (d.partial ? '-' : 'not written yet'), d.evidence ? 'mono' : null],
    ['Area', d.area ? d.area + ' (hint, not verified)' : '-'],
  ]);

  if (d.partial) {
    IE.samples.replaceChildren(h('p', 'muted', 'Loading samples…'));
    setText(IE.entry, '');
    return;
  }
  const entry = String(d.entry || '').replace(/\r\n/g, '\n');
  setText(IE.entry, entry || '(no entry yet)');
  renderSamples(d);
}

function renderIssueActions() {
  const state = I.detail ? I.detail.summary.state : '';
  const acts = ISSUE_ACTIONS[state] || Object.keys(ACTION_INFO);
  const key = acts.join(',');
  if (IE.actions._k !== key) {
    IE.actions._k = key;
    const f = document.createDocumentFragment();
    for (const a of acts) {
      const b = h('button', null, ACTION_INFO[a][0]);
      b.type = 'button';
      b.title = ACTION_INFO[a][1];
      b.addEventListener('click', () => issueAction(a));
      f.append(b);
    }
    IE.actions.replaceChildren(f);
  }
  for (const b of IE.actions.children) b.disabled = I.acting;
}

function trackOpen(det, key) {
  det.open = I.open.has(key);
  det.addEventListener('toggle', () => { if (det.open) I.open.add(key); else I.open.delete(key); });
}

function renderSamples(d) {
  const fp = d.summary.fingerprint;
  const samples = (d.samples || []).slice().sort((a, b) => b.at - a.at);
  const key = fp + JSON.stringify(samples);
  if (key === I.sampleKey) return;
  I.sampleKey = key;
  if (I.initFor !== fp) { // first look at this issue: the newest sample is open, nothing else
    I.initFor = fp;
    I.open.clear();
    if (samples.length) I.open.add(sampleKey(fp, samples[0]));
  }
  const f = document.createDocumentFragment();
  if (!samples.length) f.append(h('p', 'muted', 'No samples kept.'));
  for (const s of samples) f.append(sampleEl(d.summary, s));
  IE.samples.replaceChildren(f);
}

function sampleKey(fp, s) { return fp + '|' + s.at + '|' + s.bot; }

function sampleEl(sum, s) {
  const key = sampleKey(sum.fingerprint, s);
  const det = h('details', 'sample');
  trackOpen(det, key);
  det.append(h('summary', null, (s.bot || 'unknown bot') + ' · ' + fmtStamp(s.at)));
  const body = h('div', 's-body');
  if (s.context) body.append(h('p', 's-ctx', s.context));
  const rows = [['Expected', s.expected || '-'], ['Actual', s.actual || '-']];
  const pos = samplePos(s);
  if (pos) {
    const cell = h('span');
    const go = h('button', 'link', 'Show on map');
    go.type = 'button';
    go.addEventListener('click', () => showOnMap(pos, words(sum.kind) + ' · ' + (s.bot || sum.fingerprint)));
    cell.append(document.createTextNode(pos.x + ', ' + pos.y + ', ' + pos.z + '  '), go);
    rows.push(['Position', cell]);
  }
  for (const kv of s.facts || []) rows.push([String(kv[0]), String(kv[1])]);
  const dl = h('dl', 'kv');
  fillKv(dl, rows);
  body.append(dl);
  for (const [title, lines, tag] of [['Packet trail', s.trail, 'trail'], ['Server log', s.server_log, 'server'], ['Bot log', s.bot_log, 'bot']]) {
    if (!lines || !lines.length) continue;
    const blk = h('details', 'blk-wrap');
    trackOpen(blk, key + '#' + tag);
    const pre = h('pre', 'blk', lines.map(String).join('\n'));
    pre.tabIndex = 0;
    blk.append(h('summary', null, title + ' (' + lines.length + ')'), pre);
    body.append(blk);
  }
  det.append(body);
  return det;
}

async function issueAction(action) {
  const fp = I.sel;
  if (!fp || I.acting) return;
  I.acting = true;
  renderIssueActions();
  const label = ACTION_INFO[action][0];
  setStatus(IE.astatus, '', label + '…');
  const r = await postJson('/api/issues/' + encodeURIComponent(fp) + '/action', { action });
  I.acting = false;
  renderIssueActions();
  if (r.ok) {
    setStatus(IE.astatus, 'ok', label + ': done');
    last.issues = -Infinity;
    tick();
  } else {
    setStatus(IE.astatus, 'err', r.error);
    IE.astatus.scrollIntoView({ block: 'nearest' }); // a 401 moves the focus down to the token field
  }
}

async function pollIssues() {
  busy.issues = true;
  last.issues = performance.now();
  const fp = I.sel;
  try {
    if (fp) {
      const d = await fetchJson('/api/issues/' + encodeURIComponent(fp));
      if (I.sel !== fp || !d || !d.summary || selecting(IE.detailview)) return;
      d.summary = cleanSummary(d.summary);
      setText(IE.derr, '');
      renderIssueDetail(d);
    } else {
      const arr = await fetchJson('/api/issues');
      if (I.sel || !Array.isArray(arr) || selecting(IE.list)) return;
      I.list = arr.map(cleanSummary).filter((it) => it.fingerprint);
      I.loaded = true;
      setStatus(IE.status, '', '');
      renderIssueList();
    }
  } catch (e) {
    if (fp && I.sel === fp && e && e.status === 404) {
      closeIssue();
      setStatus(IE.status, 'err', 'That issue is gone.');
      last.issues = performance.now(); // keep the message for a poll
    } else if (fp) {
      if (I.sel === fp) setText(IE.derr, 'Could not refresh: ' + ((e && e.message) || e));
    } else {
      setStatus(IE.status, 'err', 'Could not load the issues: ' + ((e && e.message) || e));
    }
  } finally { busy.issues = false; }
}

// "Show on map": centre there and draw a pulsing marker for a while.
function showOnMap(pos, label) {
  setFollow(false);
  const f = S.flash = { x: pos.x, y: pos.y, label, t0: performance.now(), until: performance.now() + FLASH_MS };
  setTimeout(() => { if (S.flash === f) { S.flash = null; requestDraw(); } }, FLASH_MS + 50);
  centerOn({ dx: pos.x, dy: pos.y });
}

// ------------------------------------------------------------------ map: view maths
const cv = $('map');
const ctx = cv.getContext('2d', { alpha: false });
const reduceMotion = matchMedia('(prefers-reduced-motion: reduce)');
const wrap = $('mapwrap');

function worldRect() {
  const w = S.map.info && S.map.info.world;
  if (w && w.max_x > w.min_x && w.max_y > w.min_y) return w;
  return { min_x: 0, min_y: 0, max_x: 65536, max_y: 40960 };
}
function minScale() {
  const w = worldRect();
  return 0.4 * Math.min(S.W / (w.max_x - w.min_x), S.H / (w.max_y - w.min_y));
}
function clampScale(s) { return clamp(s, Math.max(minScale(), 1e-5), MAX_SCALE); }
function dotRadius() { return Math.min(7, 3.5 + Math.log2(1 + S.scale * 3)); }

function zoomAt(factor, px, py) {
  S.vanim = null;
  const wx = S.cx + (px - S.W / 2) / S.scale, wy = S.cy - (py - S.H / 2) / S.scale;
  S.scale = clampScale(S.scale * factor);
  S.cx = wx - (px - S.W / 2) / S.scale;
  S.cy = wy + (py - S.H / 2) / S.scale;
  S.userMoved = true;
  requestDraw();
}

function fitBounds(x0, y0, x1, y1, pad) {
  const w = Math.max(x1 - x0, 60), hgt = Math.max(y1 - y0, 60);
  S.vanim = null;
  S.scale = clampScale(Math.min((S.W - 2 * pad) / w, (S.H - 2 * pad) / hgt));
  S.cx = (x0 + x1) / 2;
  S.cy = (y0 + y1) / 2;
  requestDraw();
}

function fitAll() {
  if (!S.W || !S.H) return;
  let x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity, n = 0;
  for (const b of S.list) {
    if (!b.vis || (b.x === 0 && b.y === 0)) continue;
    n++;
    if (b.x < x0) x0 = b.x; if (b.x > x1) x1 = b.x;
    if (b.y < y0) y0 = b.y; if (b.y > y1) y1 = b.y;
  }
  const pad = Math.min(60, Math.min(S.W, S.H) / 6);
  if (n) {
    fitBounds(x0, y0, x1, y1, pad);
    S.scale = Math.min(S.scale, 12);
    S.fitted = true;
  } else {
    const w = worldRect();
    fitBounds(w.min_x, w.min_y, w.max_x, w.max_y, 10);
  }
}

function centerOn(b) {
  S.vanim = {
    t0: performance.now(), dur: 320, cx0: S.cx, cy0: S.cy, s0: S.scale,
    cx1: b.dx, cy1: b.dy, s1: clampScale(Math.max(S.scale, 0.8)),
  };
  S.userMoved = true;
  requestDraw();
}

function resize() {
  const r = wrap.getBoundingClientRect();
  const w = Math.max(1, Math.round(r.width)), hgt = Math.max(1, Math.round(r.height));
  const dpr = window.devicePixelRatio || 1;
  if (w !== S.W || hgt !== S.H || dpr !== S.dpr) {
    S.W = w; S.H = hgt; S.dpr = dpr;
    cv.width = Math.round(w * dpr);
    cv.height = Math.round(hgt * dpr);
  }
  if (!S.userMoved && !S.fitted) fitAll();
  requestDraw();
}

// ------------------------------------------------------------------ map: drawing
let rafId = 0;
function requestDraw() {
  if (!rafId) rafId = requestAnimationFrame(frame);
}

function frame(now) {
  rafId = 0;
  if (S.vanim) {
    const a = S.vanim;
    let t = (now - a.t0) / a.dur;
    if (t >= 1) { t = 1; S.vanim = null; }
    const e = 1 - Math.pow(1 - t, 3);
    S.cx = a.cx0 + (a.cx1 - a.cx0) * e;
    S.cy = a.cy0 + (a.cy1 - a.cy0) * e;
    S.scale = a.s0 * Math.pow(a.s1 / a.s0, e);
  }
  if (S.posDirty || now < S.animUntil) {
    const p = clamp((now - S.pollT) / S.animDur, 0, 1);
    for (const b of S.list) {
      b.dx = b.fx + (b.tx - b.fx) * p;
      b.dy = b.fy + (b.ty - b.fy) * p;
    }
    if (p >= 1) S.posDirty = false;
  }
  if (S.follow && S.sel != null && !S.vanim) {
    const b = S.bots.get(S.sel);
    if (b) { S.cx = b.dx; S.cy = b.dy; }
  }
  draw(now);
  if (S.vanim || S.posDirty || (S.flash && !reduceMotion.matches) || now < S.animUntil) requestDraw();
}

function niceStep(minPx) {
  const raw = minPx / S.scale;
  const pow = Math.pow(10, Math.floor(Math.log10(raw)));
  for (const m of [1, 2, 5, 10]) if (m * pow >= raw) return m * pow;
  return 10 * pow;
}

function drawBackdrop() {
  const W = S.W, H = S.H, sc = S.scale;
  const wr = worldRect();
  const img = S.map.img;
  const left = (wr.min_x - S.cx) * sc + W / 2, top = (S.cy - wr.max_y) * sc + H / 2;
  const w = (wr.max_x - wr.min_x) * sc, hgt = (wr.max_y - wr.min_y) * sc;
  if (img) {
    // Draw only the visible part of the image: huge destination rectangles are slow and imprecise.
    const x0 = Math.max(0, left), y0 = Math.max(0, top), x1 = Math.min(W, left + w), y1 = Math.min(H, top + hgt);
    if (x1 > x0 && y1 > y0) {
      const iw = img.naturalWidth, ih = img.naturalHeight;
      ctx.drawImage(img, (x0 - left) / w * iw, (y0 - top) / hgt * ih, (x1 - x0) / w * iw, (y1 - y0) / hgt * ih,
        x0, y0, x1 - x0, y1 - y0);
    }
  } else {
    const step = niceStep(90);
    const xa = S.cx - W / 2 / sc, xb = S.cx + W / 2 / sc, ya = S.cy - H / 2 / sc, yb = S.cy + H / 2 / sc;
    ctx.lineWidth = 1;
    ctx.strokeStyle = C.grid;
    ctx.fillStyle = C['grid-text'];
    ctx.font = '11px ui-monospace, Consolas, monospace';
    ctx.textBaseline = 'top';
    ctx.textAlign = 'left';
    ctx.beginPath();
    for (let x = Math.ceil(xa / step) * step; x <= xb; x += step) {
      const sx = Math.round((x - S.cx) * sc + W / 2) + 0.5;
      ctx.moveTo(sx, 0); ctx.lineTo(sx, H);
    }
    for (let y = Math.ceil(ya / step) * step; y <= yb; y += step) {
      const sy = Math.round((S.cy - y) * sc + H / 2) + 0.5;
      ctx.moveTo(0, sy); ctx.lineTo(W, sy);
    }
    ctx.stroke();
    for (let x = Math.ceil(xa / step) * step; x <= xb; x += step) {
      ctx.fillText(nf.format(Math.round(x)), (x - S.cx) * sc + W / 2 + 3, 46);
    }
    ctx.textBaseline = 'bottom';
    for (let y = Math.ceil(ya / step) * step; y <= yb; y += step) {
      ctx.fillText(nf.format(Math.round(y)), 4, (S.cy - y) * sc + H / 2 - 2);
    }
  }
  // world border
  ctx.strokeStyle = C.muted;
  ctx.lineWidth = 1;
  ctx.setLineDash([6, 4]);
  ctx.strokeRect(left + 0.5, top + 0.5, w, hgt);
  ctx.setLineDash([]);
}

function haloText(text, x, y) {
  ctx.lineWidth = 3;
  ctx.strokeStyle = C.halo;
  ctx.strokeText(text, x, y);
  ctx.fillStyle = C.label;
  ctx.fillText(text, x, y);
}

function drawMarkers() {
  const W = S.W, H = S.H, sc = S.scale, cx = S.cx, cy = S.cy;
  const ms = S.markers;
  if (!ms.length) return;
  ctx.lineWidth = 1.25;
  ctx.strokeStyle = 'rgba(0,0,0,0.55)';
  for (const kind of MARKER_KINDS) {
    const style = MARKER[kind];
    if (sc < style.minScale) continue;
    ctx.fillStyle = style.color;
    ctx.beginPath();
    for (const m of ms) {
      if (m.kind !== kind) continue;
      const x = (m.x - cx) * sc + W / 2, y = (cy - m.y) * sc + H / 2;
      if (x < -12 || x > W + 12 || y < -12 || y > H + 12) continue;
      if (kind === 'house') ctx.rect(x - 3.5, y - 3.5, 7, 7);
      else if (kind === 'duel_flag') { ctx.moveTo(x - 1, y + 6); ctx.lineTo(x - 1, y - 6); ctx.lineTo(x + 7, y - 3); ctx.lineTo(x - 1, y); }
      else if (kind === 'trade_outlet') { ctx.moveTo(x, y - 6); ctx.lineTo(x + 5, y); ctx.lineTo(x, y + 6); ctx.lineTo(x - 5, y); ctx.closePath(); }
      else { ctx.moveTo(x + 4.5, y); ctx.arc(x, y, 4.5, 0, TAU); }
    }
    ctx.fill();
    ctx.stroke();
  }
  if (sc >= 1.5) {
    ctx.font = '11px system-ui, sans-serif';
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'left';
    ctx.lineJoin = 'round';
    let n = 0;
    for (const m of ms) {
      if (!m.label || n > 200) continue;
      const x = (m.x - cx) * sc + W / 2, y = (cy - m.y) * sc + H / 2;
      if (x < -12 || x > W + 12 || y < -12 || y > H + 12) continue;
      haloText(m.label, x + 9, y);
      n++;
    }
  }
}

function drawLines() {
  const W = S.W, H = S.H, sc = S.scale, cx = S.cx, cy = S.cy;
  ctx.lineWidth = 1.5;
  ctx.setLineDash([5, 4]);
  for (const b of S.list) {
    if (b.partner == null || !b.vis) continue;
    const p = S.bots.get(b.partner);
    if (!p || (p.partner === b.id && p.id < b.id) || (b.x === 0 && b.y === 0) || (p.x === 0 && p.y === 0)) continue;
    const x0 = (b.dx - cx) * sc + W / 2, y0 = (cy - b.dy) * sc + H / 2;
    const x1 = (p.dx - cx) * sc + W / 2, y1 = (cy - p.dy) * sc + H / 2;
    if ((x0 < 0 && x1 < 0) || (x0 > W && x1 > W) || (y0 < 0 && y1 < 0) || (y0 > H && y1 > H)) continue;
    ctx.strokeStyle = ACT_COLORS[b.ai];
    ctx.beginPath();
    ctx.moveTo(x0, y0);
    ctx.lineTo(x1, y1);
    ctx.stroke();
  }
  ctx.setLineDash([]);
}

// The route the selected bot is walking (BotDetail.path: world [x, y] points, the bot first): a thin
// line with a dot at the far end. Nothing is drawn while the bot stands or walks the straight line
// without a route.
function drawPath() {
  const d = S.detail;
  if (S.sel == null || !d || !d.snapshot || d.snapshot.id !== S.sel) return;
  const p = Array.isArray(d.path) ? d.path : [];
  if (p.length < 2) return;
  const W = S.W, H = S.H, sc = S.scale, cx = S.cx, cy = S.cy;
  ctx.lineWidth = 1.5;
  ctx.lineJoin = 'round';
  ctx.strokeStyle = C.accent;
  ctx.globalAlpha = 0.85;
  ctx.beginPath();
  for (let i = 0; i < p.length; i++) {
    const x = (num(p[i][0]) - cx) * sc + W / 2, y = (cy - num(p[i][1])) * sc + H / 2;
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  }
  ctx.stroke();
  const e = p[p.length - 1];
  ctx.beginPath();
  ctx.arc((num(e[0]) - cx) * sc + W / 2, (cy - num(e[1])) * sc + H / 2, 3, 0, TAU);
  ctx.fillStyle = C.accent;
  ctx.fill();
  ctx.globalAlpha = 1;
}

function drawBots() {
  const W = S.W, H = S.H, sc = S.scale, cx = S.cx, cy = S.cy;
  const n = S.list.length;
  if (S.sx.length < n) {
    S.sx = new Float32Array(n + 256);
    S.sy = new Float32Array(n + 256);
    S.on = new Uint8Array(n + 256);
  }
  const r = dotRadius();
  const margin = r + 6;
  let onCount = 0;
  for (let i = 0; i < n; i++) {
    const b = S.list[i];
    const sx = (b.dx - cx) * sc + W / 2, sy = (cy - b.dy) * sc + H / 2;
    S.sx[i] = sx; S.sy[i] = sy;
    const on = b.vis && (b.x !== 0 || b.y !== 0) && sx > -margin && sx < W + margin && sy > -margin && sy < H + margin;
    S.on[i] = on ? 1 : 0;
    if (on) onCount++;
  }
  ctx.lineWidth = 1;
  for (let a = 0; a < ACTIVITIES.length; a++) {
    if (!S.counts[a]) continue;
    ctx.fillStyle = ACT_COLORS[a];
    ctx.strokeStyle = a === ACT_INDEX.dead ? '#ffffff' : 'rgba(0,0,0,0.5)';
    ctx.globalAlpha = a === 0 ? 0.5 : 1;
    ctx.beginPath();
    for (let i = 0; i < n; i++) {
      if (S.on[i] && S.list[i].ai === a) { ctx.moveTo(S.sx[i] + r, S.sy[i]); ctx.arc(S.sx[i], S.sy[i], r, 0, TAU); }
    }
    ctx.fill();
    ctx.stroke();
  }
  ctx.globalAlpha = 1;

  // selected ring (drawn even when filtered out)
  const sel = S.sel != null ? S.bots.get(S.sel) : null;
  if (sel) {
    const x = (sel.dx - cx) * sc + W / 2, y = (cy - sel.dy) * sc + H / 2;
    ctx.lineWidth = 2.5;
    ctx.strokeStyle = C.accent;
    ctx.beginPath();
    ctx.arc(x, y, r + 5, 0, TAU);
    ctx.stroke();
    ctx.lineWidth = 1.5;
    ctx.strokeStyle = C.halo;
    ctx.beginPath();
    ctx.arc(x, y, r + 7.5, 0, TAU);
    ctx.stroke();
  }
  const hov = S.hover;
  if (hov && hov !== sel) {
    const x = (hov.dx - cx) * sc + W / 2, y = (cy - hov.dy) * sc + H / 2;
    ctx.lineWidth = 2;
    ctx.strokeStyle = C.label;
    ctx.beginPath();
    ctx.arc(x, y, r + 3.5, 0, TAU);
    ctx.stroke();
  }

  // names
  if (S.showNames) {
    ctx.font = '11px system-ui, sans-serif';
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'left';
    ctx.lineJoin = 'round';
    if (sc >= NAME_SCALE || onCount <= NAME_MAX_BOTS) {
      let drawn = 0;
      for (let i = 0; i < n && drawn < 300; i++) {
        if (!S.on[i]) continue;
        haloText(S.list[i].name, S.sx[i] + r + 4, S.sy[i]);
        drawn++;
      }
    } else {
      for (const b of [sel, hov]) {
        if (b) haloText(b.name, (b.dx - cx) * sc + W / 2 + r + 6, (cy - b.dy) * sc + H / 2);
      }
    }
  } else {
    ctx.font = '11px system-ui, sans-serif';
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'left';
    for (const b of [sel, hov]) {
      if (b) haloText(b.name, (b.dx - cx) * sc + W / 2 + r + 6, (cy - b.dy) * sc + H / 2);
    }
  }
}

// The temporary marker of "show on map" in the issue detail: a pulsing ring with crosshair and label.
function drawFlash(now) {
  const f = S.flash;
  if (!f) return;
  if (now > f.until) { S.flash = null; return; }
  const x = (f.x - S.cx) * S.scale + S.W / 2, y = (S.cy - f.y) * S.scale + S.H / 2;
  if (x < -40 || x > S.W + 40 || y < -40 || y > S.H + 40) return;
  const t = reduceMotion.matches ? 0 : (Math.max(0, now - f.t0) % 1400) / 1400;
  ctx.beginPath();
  ctx.arc(x, y, 9, 0, TAU);
  ctx.moveTo(x - 17, y); ctx.lineTo(x - 5, y);
  ctx.moveTo(x + 5, y); ctx.lineTo(x + 17, y);
  ctx.moveTo(x, y - 17); ctx.lineTo(x, y - 5);
  ctx.moveTo(x, y + 5); ctx.lineTo(x, y + 17);
  ctx.lineWidth = 5.5; // a light/dark outline first, so the red shows on every kind of terrain
  ctx.strokeStyle = C.halo;
  ctx.stroke();
  ctx.lineWidth = 2.5;
  ctx.strokeStyle = C.danger;
  ctx.stroke();
  if (t > 0) {
    ctx.globalAlpha = 1 - t;
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(x, y, 9 + 24 * t, 0, TAU);
    ctx.stroke();
    ctx.globalAlpha = 1;
  }
  ctx.font = '11px system-ui, sans-serif';
  ctx.textBaseline = 'middle';
  ctx.textAlign = 'left';
  ctx.lineJoin = 'round';
  haloText(f.label, x + 21, y);
}

function draw(now) {
  const dpr = S.dpr;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.fillStyle = C['map-bg'];
  ctx.fillRect(0, 0, S.W, S.H);
  drawBackdrop();
  if (S.showMarkers) drawMarkers();
  if (S.showLines) drawLines();
  drawPath();
  drawBots();
  drawFlash(now);
  updateHover();
}

// ------------------------------------------------------------------ map: hover, picking, tooltip
function pick(px, py, touch) {
  const r = dotRadius() + (touch ? 12 : 5);
  let best = null, bd = r * r;
  for (let i = 0; i < S.list.length; i++) {
    if (!S.on[i]) continue;
    const dx = S.sx[i] - px, dy = S.sy[i] - py, d = dx * dx + dy * dy;
    if (d <= bd) { bd = d; best = S.list[i]; }
  }
  if (best) return { bot: best, marker: null };
  if (!S.showMarkers) return { bot: null, marker: null };
  const mr = touch ? 16 : 9;
  let bm = null, md = mr * mr;
  for (const m of S.markers) {
    if (S.scale < MARKER[m.kind].minScale) continue;
    const dx = (m.x - S.cx) * S.scale + S.W / 2 - px, dy = (S.cy - m.y) * S.scale + S.H / 2 - py, d = dx * dx + dy * dy;
    if (d <= md) { md = d; bm = m; }
  }
  return { bot: null, marker: bm };
}

const tip = { el: $('tip'), name: $('tip-name'), sub: $('tip-sub'), goal: $('tip-goal') };

function updateHover() {
  if (!S.mouse.in || S.dragging) {
    if (S.hover || S.hoverMarker || !tip.el.hidden) { S.hover = null; S.hoverMarker = null; tip.el.hidden = true; cv.classList.remove('pointing'); }
    return;
  }
  const hit = pick(S.mouse.x, S.mouse.y, S.mouse.touch);
  S.hover = hit.bot;
  S.hoverMarker = hit.marker;
  cv.classList.toggle('pointing', !!(hit.bot || hit.marker));
  if (!hit.bot && !hit.marker) { tip.el.hidden = true; return; }
  if (hit.bot) {
    const b = hit.bot;
    setText(tip.name, b.name);
    setText(tip.sub, 'Lv ' + b.level + ' ' + b.cls + ' · ' + ACT[b.activity].label);
    setText(tip.goal, b.detail || b.goal);
  } else {
    setText(tip.name, hit.marker.label || MARKER[hit.marker.kind].label);
    setText(tip.sub, MARKER[hit.marker.kind].label);
    setText(tip.goal, '');
  }
  tip.el.hidden = false;
  const tw = tip.el.offsetWidth, th = tip.el.offsetHeight;
  let x = S.mouse.x + 14, y = S.mouse.y + 14;
  if (x + tw > S.W - 4) x = S.mouse.x - tw - 12;
  if (y + th > S.H - 4) y = S.mouse.y - th - 12;
  tip.el.style.transform = 'translate(' + Math.max(2, Math.round(x)) + 'px,' + Math.max(2, Math.round(y)) + 'px)';
}

function showCoords(px, py) {
  const c = $('coords');
  if (px == null) { c.hidden = true; return; }
  c.hidden = false;
  const wx = S.cx + (px - S.W / 2) / S.scale, wy = S.cy - (py - S.H / 2) / S.scale;
  setText(c, 'x ' + nf.format(Math.round(wx)) + '  y ' + nf.format(Math.round(wy)) + '   ' + (S.scale >= 1 ? S.scale.toFixed(1) : S.scale.toFixed(3)) + ' px/m');
}

// ------------------------------------------------------------------ map: input
function bindMap() {
  const ptrs = new Map();
  let drag = null, pinch = null;

  cv.addEventListener('pointerdown', (e) => {
    cv.focus({ preventScroll: true });
    cv.setPointerCapture(e.pointerId);
    ptrs.set(e.pointerId, { x: e.offsetX, y: e.offsetY });
    S.vanim = null;
    S.mouse.touch = e.pointerType === 'touch';
    if (ptrs.size === 1) {
      drag = { x: e.offsetX, y: e.offsetY, lx: e.offsetX, ly: e.offsetY, moved: false };
      pinch = null;
    } else if (ptrs.size === 2) {
      const [a, b] = [...ptrs.values()];
      pinch = { d: Math.hypot(a.x - b.x, a.y - b.y) || 1, mx: (a.x + b.x) / 2, my: (a.y + b.y) / 2 };
      if (drag) drag.moved = true;
    }
  });

  cv.addEventListener('pointermove', (e) => {
    const px = e.offsetX, py = e.offsetY;
    S.mouse.x = px; S.mouse.y = py; S.mouse.in = true; S.mouse.touch = e.pointerType === 'touch';
    if (ptrs.has(e.pointerId)) ptrs.set(e.pointerId, { x: px, y: py });
    if (ptrs.size >= 2 && pinch) {
      const [a, b] = [...ptrs.values()];
      const d = Math.hypot(a.x - b.x, a.y - b.y) || 1, mx = (a.x + b.x) / 2, my = (a.y + b.y) / 2;
      S.cx -= (mx - pinch.mx) / S.scale;
      S.cy += (my - pinch.my) / S.scale;
      zoomAt(d / pinch.d, mx, my);
      pinch.d = d; pinch.mx = mx; pinch.my = my;
      S.dragging = true;
    } else if (drag && ptrs.has(e.pointerId)) {
      if (!drag.moved && Math.hypot(px - drag.x, py - drag.y) > 4) drag.moved = true;
      if (drag.moved) {
        S.dragging = true;
        cv.classList.add('dragging');
        S.cx -= (px - drag.lx) / S.scale;
        S.cy += (py - drag.ly) / S.scale;
        if (S.follow) setFollow(false);
        S.userMoved = true;
      }
      drag.lx = px; drag.ly = py;
    }
    showCoords(px, py);
    requestDraw();
  });

  const end = (e) => {
    const was = ptrs.get(e.pointerId);
    ptrs.delete(e.pointerId);
    if (e.type === 'pointerup' && was && drag && !drag.moved && ptrs.size === 0) {
      const hit = pick(e.offsetX, e.offsetY, e.pointerType === 'touch');
      if (hit.bot) select(hit.bot.id, { reveal: true, openPanel: true });
      else if (!hit.marker) select(null);
    }
    if (ptrs.size === 0) { drag = null; pinch = null; S.dragging = false; cv.classList.remove('dragging'); }
    else if (ptrs.size === 1) {
      const p = [...ptrs.values()][0];
      drag = { x: p.x, y: p.y, lx: p.x, ly: p.y, moved: true };
      pinch = null;
    }
    if (e.pointerType === 'touch') { S.mouse.in = false; showCoords(null); }
    requestDraw();
  };
  cv.addEventListener('pointerup', end);
  cv.addEventListener('pointercancel', end);
  cv.addEventListener('pointerleave', (e) => {
    if (e.pointerType === 'mouse' && !ptrs.size) { S.mouse.in = false; showCoords(null); requestDraw(); }
  });
  cv.addEventListener('wheel', (e) => {
    e.preventDefault();
    let dy = e.deltaY;
    if (e.deltaMode === 1) dy *= 33; else if (e.deltaMode === 2) dy *= 400;
    zoomAt(Math.exp(-dy * (e.ctrlKey ? 0.01 : 0.0016)), e.offsetX, e.offsetY);
  }, { passive: false });
  cv.addEventListener('dblclick', (e) => zoomAt(2, e.offsetX, e.offsetY));

  $('z-in').addEventListener('click', () => zoomAt(1.6, S.W / 2, S.H / 2));
  $('z-out').addEventListener('click', () => zoomAt(1 / 1.6, S.W / 2, S.H / 2));
  $('z-fit').addEventListener('click', () => { S.userMoved = false; fitAll(); });
  bindToggle('o-follow', 'pb_follow', false, (v) => { S.follow = v; requestDraw(); });
  bindToggle('o-markers', 'pb_markers', true, (v) => { S.showMarkers = v; requestDraw(); });
  bindToggle('o-lines', 'pb_lines', true, (v) => { S.showLines = v; requestDraw(); });
  bindToggle('o-names', 'pb_names', true, (v) => { S.showNames = v; requestDraw(); });

  document.addEventListener('keydown', (e) => {
    const t = e.target;
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    if (t && (t.tagName === 'INPUT' || t.tagName === 'SELECT' || t.tagName === 'TEXTAREA')) {
      if (e.key === 'Escape' && t.type === 'search') { t.value = ''; S.search = ''; refreshView(); }
      return;
    }
    if (e.key === '+' || e.key === '=') zoomAt(1.4, S.W / 2, S.H / 2);
    else if (e.key === '-' || e.key === '_') zoomAt(1 / 1.4, S.W / 2, S.H / 2);
    else if (e.key === 'f' || e.key === 'F') { S.userMoved = false; fitAll(); }
    else if (e.key === 'Escape') { if (I.tab && I.sel) closeIssue(); else select(null); }
  });
}

function setFollow(v) {
  S.follow = v;
  $('o-follow').checked = v;
}

function bindToggle(id, key, dflt, apply) {
  const box = $(id);
  const saved = lsGet(key);
  const v = saved == null ? dflt : saved === '1';
  box.checked = v;
  apply(v);
  box.addEventListener('change', () => { lsSet(key, box.checked ? '1' : '0'); apply(box.checked); });
}

// ------------------------------------------------------------------ polling
const last = { bots: -Infinity, markers: -Infinity, detail: -Infinity, map: -Infinity, issues: -Infinity };
const busy = { bots: false, markers: false, detail: false, map: false, issues: false };

async function fetchJson(url, ms) {
  const ac = new AbortController();
  const t = setTimeout(() => ac.abort(), ms || 4000);
  try {
    const r = await fetch(url, { signal: ac.signal, cache: 'no-store' });
    if (!r.ok) { const e = new Error('HTTP ' + r.status); e.status = r.status; throw e; }
    return await r.json();
  } finally { clearTimeout(t); }
}

async function pollBots() {
  busy.bots = true;
  last.bots = performance.now();
  try {
    const [bots, stats] = await Promise.allSettled([fetchJson('/api/bots'), fetchJson('/api/stats')]);
    if (bots.status === 'fulfilled' && Array.isArray(bots.value)) {
      applySnapshots(bots.value, performance.now());
      refreshView();
      if (!S.fitted && !S.userMoved && S.W) fitAll();
      if (!S.restoredHash) restoreHash();
      if (S.sel != null) renderDetail();
      S.failures = 0;
      S.lastOkAt = performance.now();
    } else {
      S.failures++;
    }
    if (stats.status === 'fulfilled') { S.stats = stats.value; renderStats(); }
  } finally { busy.bots = false; renderConn(); }
}

async function pollMarkers() {
  busy.markers = true;
  last.markers = performance.now();
  try {
    const ms = await fetchJson('/api/markers');
    if (Array.isArray(ms)) {
      S.markers = ms.filter((m) => MARKER[m.kind]).map((m) => ({ kind: m.kind, x: num(m.x), y: num(m.y), label: m.label || '' }));
      requestDraw();
    }
  } catch (_) { /* markers are optional */ } finally { busy.markers = false; }
}

async function pollMap() {
  busy.map = true;
  last.map = performance.now();
  try {
    const info = await fetchJson('/api/map');
    const key = JSON.stringify(info);
    if (key === S.map.key) return;
    S.map.key = key;
    S.map.info = info;
    S.map.img = null;
    if (info.image_url) {
      const img = new Image();
      img.onload = () => { if (S.map.key === key) { S.map.img = img; requestDraw(); } };
      img.src = info.image_url;
    }
    if (!S.userMoved && !S.fitted) fitAll();
    requestDraw();
  } catch (_) { /* grid fallback */ } finally { busy.map = false; }
}

async function pollDetail() {
  const id = S.sel;
  if (id == null) return;
  busy.detail = true;
  last.detail = performance.now();
  try {
    const d = await fetchJson('/api/bots/' + id);
    if (S.sel === id) { S.detail = d; renderDetail(); requestDraw(); }
  } catch (e) {
    if (S.sel === id && e && e.status === 404) {
      select(null);
    }
  } finally { busy.detail = false; }
}

function tick() {
  if (document.visibilityState !== 'visible') return;
  const now = performance.now();
  if (!busy.bots && now - last.bots >= POLL.bots) pollBots();
  if (!busy.markers && now - last.markers >= POLL.markers) pollMarkers();
  if (!busy.map && now - last.map >= POLL.map) pollMap();
  if (!busy.detail && S.sel != null && now - last.detail >= POLL.detail) pollDetail();
  if (!busy.issues && issuesVisible() && now - last.issues >= POLL.issues) pollIssues();
}

function restoreHash() {
  S.restoredHash = true;
  const m = /^#bot=(\d+)$/.exec(location.hash);
  if (!m) return;
  const id = Number(m[1]);
  if (S.bots.has(id)) { setTab(false); select(id, { center: true, reveal: true }); }
}

// ------------------------------------------------------------------ boot
function setPanel(open) {
  document.body.classList.toggle('panel-hidden', !open);
  $('btn-panel').setAttribute('aria-expanded', open ? 'true' : 'false');
  lsSet('pb_panel', open ? '1' : '0');
  last.issues = -Infinity; // refresh the issues at once when the panel opens
}

function init() {
  readTheme();
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => { readTheme(); requestDraw(); });
  try {
    const s = JSON.parse(lsGet('pb_sort') || 'null');
    if (s && SORTERS[s.key] && (s.dir === 1 || s.dir === -1)) S.sort = s;
  } catch (_) { /* default sort */ }

  buildChips();
  buildLegend();
  buildList();
  buildDetail();
  buildGlobal();
  buildIssues();
  bindMap();

  let searchTimer = 0;
  $('search').addEventListener('input', (e) => {
    clearTimeout(searchTimer);
    searchTimer = setTimeout(() => { S.search = e.target.value; refreshView(); }, 120);
  });
  $('btn-panel').addEventListener('click', () => setPanel(document.body.classList.contains('panel-hidden')));
  setPanel(lsGet('pb_panel') !== '0');

  new ResizeObserver(resize).observe(wrap);
  window.addEventListener('resize', resize);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') {
      last.bots = last.markers = last.detail = last.map = last.issues = -Infinity;
      tick();
      requestDraw();
    }
  });
  window.addEventListener('hashchange', () => { S.restoredHash = false; restoreHash(); });
  setInterval(tick, 200);
  setInterval(renderConn, 1000);
  resize();
  tick();
}

init();
})();
