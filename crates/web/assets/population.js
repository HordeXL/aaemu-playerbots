/* Playerbots operator UI: the World, Economy and Director views, and the heat overlay of the map.
 *
 * Runs after app.js and borrows its helpers through window.pbHost. Polls /api/world (3 s, only while
 * the World view or the heat overlay is on screen), /api/economy (5 s, Economy view) and
 * /api/director (2 s on the Director and Economy views, 5 s otherwise, for the mode badge).
 * Changes go out as POST /api/command (spawn_where / despawn_where) and POST /api/director/policy.
 * Every string that comes from the bot process is inserted with textContent, never innerHTML.
 */
(() => {
'use strict';
const host = window.pbHost;
if (!host) return;
const { h, setText, num, fmtCount, fmtDur, fmtBytes, fmtClock, lsGet, lsSet, fetchJson, postJson, setStatus } = host;
const $ = (id) => document.getElementById(id);

// ------------------------------------------------------------------ constants
const SIDES = ['nuia', 'haranya'];
const SIDE_LABEL = { nuia: 'Nuia', haranya: 'Haranya', other: 'Other', unknown: '?' };
const CONTINENTS = ['west', 'east', 'auroria', 'sea', 'unknown'];
const CONT_LABEL = { west: 'West', east: 'East', auroria: 'Auroria', sea: 'Sea', unknown: 'Unknown' };
const HOME = { nuia: 'west', haranya: 'east' };
const RACES = [['nuian', 'Nuian'], ['elf', 'Elf'], ['hariharan', 'Hariharan'], ['ferre', 'Ferre']];
const POLL = { world: 3000, economy: 5000, directorFast: 2000, directorSlow: 5000 };
const HEAT_CELL = 512;
const MODE_LABEL = { off: 'Off', dry_run: 'Dry run', live: 'Live', holding: 'Holding' };
const KIND_LABEL = {
  spawn: 'Start bots', despawn: 'Stop bots', hold: 'Held back', policy: 'Policy', wish: 'Wish', result: 'Done',
};
// numeric policy fields: [key, label, min, max, step, hint]
const POLICY_NUMBERS = [
  ['max_online', 'Bots online at most', 0, 5000, 1, 'All bots together; the director starts no more above this.'],
  ['min_per_side', 'Least per side', 0, 5000, 1, 'Each side keeps at least this many bots.'],
  ['per_side_cap', 'Most per side', 0, 5000, 1, 'No side gets more bots than this from the director.'],
  ['mirror_pct', 'Mirror %', 0, 1000, 5, 'A side aims for this many characters per 100 players on the other side.'],
  ['step', 'Bots per action', 1, 1000, 1, 'The most bots one action starts or stops.'],
  ['hysteresis', 'Keep above target', 0, 1000, 1, 'Bots above the target a side keeps before the director stops some.'],
  ['backoff_rss_mb', 'Back off at MB', 0, 1000000, 64, 'Start no bots above this much process memory (0 = ignore).'],
  ['backoff_packets_in', 'Back off at packets/s', 0, 100000000, 500, 'Start no bots above this many packets per second (0 = ignore).'],
  ['backoff_login_failures', 'Back off at failures', 0, 5000, 1, 'Failed or short sessions in five minutes (0 = ignore).'],
  ['backoff_open_issues', 'Back off at issues', 0, 100000, 1, 'Open server issues (0 = ignore).'],
  ['auto_boost', 'Automatic wishes', 0, 16, 1, 'The scarcest items become wishes by themselves (0 = none).'],
  ['auto_boost_min', 'Wish at scarcity', 0, 1, 0.05, 'Only items that score at least this (0 to 1).'],
];
const BIAS_GOALS = ['quest', 'grind', 'duel', 'trade', 'trade_run', 'housing', 'dungeon', 'sail', 'auction',
  'market', 'guild', 'gank', 'warzone', 'event', 'companion', 'farm', 'rest', 'wander'];

// ------------------------------------------------------------------ state
const P = {
  tab: 'bots', world: null, economy: null, director: null,
  last: { world: -Infinity, economy: -Infinity, director: -Infinity },
  busy: { world: false, economy: false, director: false },
  heat: lsGet('pb_heat') === '1' || /[#&]heat=1/.test(location.hash),
  ok: { world: true, economy: true, director: true },
};
const E = {}; // elements

// ------------------------------------------------------------------ small helpers
function label(s) {
  s = String(s || '').replace(/_/g, ' ');
  return s.charAt(0).toUpperCase() + s.slice(1);
}
function panelOpen() { return !document.body.classList.contains('panel-hidden'); }
function fmtMoney(c) {
  c = Math.round(num(c));
  if (c <= 0) return '-';
  const g = Math.floor(c / 10000), s = Math.floor((c % 10000) / 100), r = c % 100;
  const out = [];
  if (g) out.push(g + 'g');
  if (s) out.push(s + 's');
  if (r && !g) out.push(r + 'c');
  return out.join(' ');
}
function fmtAge(secs) {
  secs = Math.max(0, Math.floor(num(secs)));
  return secs < 90 ? secs + 's' : fmtDur(secs);
}
function sideSpan(side) {
  const s = SIDE_LABEL[side] ? side : 'unknown';
  return h('span', 'side ' + s, SIDE_LABEL[s]);
}
function mkTable(cls, heads) {
  const t = h('table', 'pop ' + cls);
  const tr = h('tr');
  for (const x of heads) {
    const th = h('th', x.n ? 'num' : null, x.t);
    if (x.title) th.title = x.title;
    tr.append(th);
  }
  const head = h('thead');
  head.append(tr);
  t.append(head, h('tbody'));
  return t;
}
function row(cells) {
  const tr = h('tr');
  for (const c of cells) {
    const td = h('td', c.n ? 'num' : null);
    if (c.cls) td.className += (td.className ? ' ' : '') + c.cls;
    if (c.title) td.title = c.title;
    if (c.el) td.append(c.el);
    else td.textContent = c.t == null ? '' : String(c.t);
    tr.append(td);
  }
  return tr;
}
function empty(text) { return h('div', 'pop-empty muted', text); }
function section(title, sub) {
  const e = h('h3', null, title);
  if (sub) e.append(' ', h('span', 'muted', sub));
  return e;
}
function tile(k, v, sub) {
  const t = h('div', 'tile');
  t.append(h('span', 'k', k));
  const b = h('b', null, v);
  if (sub) b.append(h('small', null, sub));
  t.append(b);
  return t;
}
function bar(frac, cls) {
  const b = h('span', 'pbar ' + (cls || ''));
  const i = h('i');
  i.style.width = Math.round(Math.min(1, Math.max(0, num(frac))) * 100) + '%';
  b.append(i);
  return b;
}
function numInput(min, max, step, value) {
  const i = h('input');
  i.type = 'number';
  i.min = min; i.max = max; i.step = step;
  if (value != null) i.value = value;
  return i;
}
function btn(text, title, fn, cls) {
  const b = h('button', cls || null, text);
  b.type = 'button';
  if (title) b.title = title;
  b.addEventListener('click', fn);
  return b;
}
function sel(options, value) {
  const s = h('select');
  for (const [v, t] of options) {
    const o = h('option', null, t);
    o.value = v;
    s.append(o);
  }
  if (value != null) s.value = value;
  return s;
}

// ------------------------------------------------------------------ polling
function due(name, ms) { return !P.busy[name] && performance.now() - P.last[name] >= ms; }

async function pollWorld() {
  P.busy.world = true; P.last.world = performance.now();
  try {
    P.world = await fetchJson('/api/world');
    P.ok.world = true;
    if (P.tab === 'world') renderWorld();
    host.requestDraw();
  } catch (e) {
    P.ok.world = false;
    if (P.tab === 'world') setStatus(E.wStatus, 'err', 'Could not load the world view: ' + ((e && e.message) || e));
  } finally { P.busy.world = false; }
}
async function pollEconomy() {
  P.busy.economy = true; P.last.economy = performance.now();
  try {
    P.economy = await fetchJson('/api/economy');
    P.ok.economy = true;
    if (P.tab === 'economy') renderEconomy();
  } catch (e) {
    P.ok.economy = false;
    if (P.tab === 'economy') setStatus(E.eStatus, 'err', 'Could not load the economy: ' + ((e && e.message) || e));
  } finally { P.busy.economy = false; }
}
async function pollDirector() {
  P.busy.director = true; P.last.director = performance.now();
  try {
    P.director = await fetchJson('/api/director');
    P.ok.director = true;
    renderBadge();
    if (P.tab === 'director') renderDirector();
    else if (P.tab === 'economy' && P.economy) renderWishes();
  } catch (e) {
    P.ok.director = false;
    if (P.tab === 'director') setStatus(E.dStatus, 'err', 'Could not load the director: ' + ((e && e.message) || e));
  } finally { P.busy.director = false; }
}

function step() {
  if (document.visibilityState !== 'visible') return;
  const open = panelOpen();
  const tab = P.tab;
  if (((tab === 'world' && open) || P.heat) && due('world', POLL.world)) pollWorld();
  if (tab === 'economy' && open && due('economy', POLL.economy)) pollEconomy();
  const fast = open && (tab === 'director' || tab === 'economy');
  if (due('director', fast ? POLL.directorFast : POLL.directorSlow)) pollDirector();
}
function kick(name) { P.last[name] = -Infinity; step(); }

// ------------------------------------------------------------------ the World view
function buildWorld() {
  const root = $('world');
  E.wTiles = h('div', 'pop-tiles');
  E.wGrid = h('div', 'pop-block');
  E.wStatus = h('div', 'status');
  E.wStatus.setAttribute('role', 'status');
  E.wZones = h('div', 'pop-block');
  E.wPlayers = h('div', 'pop-block');
  E.wMix = h('div', 'pop-block');
  E.wServer = h('div', 'pop-block');

  // the filtered spawn / despawn form
  const form = h('div', 'pop-form');
  E.fCount = numInput(1, 1000, 1, lsGet('pb_pop_count') || '3');
  E.fCount.setAttribute('aria-label', 'Number of bots');
  E.fSide = sel([['', 'Any side'], ['nuia', 'Nuia'], ['haranya', 'Haranya']]);
  E.fSide.setAttribute('aria-label', 'Side');
  E.fRace = sel([['', 'Any race']].concat(RACES));
  E.fRace.setAttribute('aria-label', 'Race');
  E.fCont = sel([['', 'Any continent'], ['west', 'West'], ['east', 'East'], ['auroria', 'Auroria'], ['sea', 'Sea']]);
  E.fCont.setAttribute('aria-label', 'Continent');
  E.fCont.title = 'Spawn: the continent the race starts on. Despawn: where the bot stands now.';
  form.append(E.fCount, E.fSide, E.fRace, E.fCont,
    btn('Spawn', 'Log in bots that match', () => askFilter('spawn_where')),
    btn('Despawn', 'Log out bots that match (idle ones first; never one that trades, duels or runs a dungeon)', () => askFilter('despawn_where')));
  E.fCount.addEventListener('change', () => lsSet('pb_pop_count', E.fCount.value));

  root.append(
    E.wTiles,
    section('Where everyone is', '(players are the ones a bot has in view)'), E.wGrid,
    section('Start or stop bots'), form, E.wStatus,
    section('Busiest zones'), E.wZones,
    section('Players in view'), E.wPlayers,
    section('What the bots do'), E.wMix,
    section('Game server'), E.wServer,
  );
}

function filterOf() {
  const f = {};
  if (E.fSide.value) f.alliance = E.fSide.value;
  if (E.fRace.value) f.race = E.fRace.value;
  if (E.fCont.value) f.continent = E.fCont.value;
  return f;
}
function describeFilter(f) {
  const bits = [];
  if (f.alliance) bits.push(SIDE_LABEL[f.alliance]);
  if (f.race) bits.push(label(f.race));
  if (f.continent) bits.push('on the ' + CONT_LABEL[f.continent] + ' continent');
  return bits.length ? bits.join(' ') : 'any';
}
async function sendFilter(type, count, filter) {
  const r = await postJson('/api/command', { type, count, filter });
  if (r.ok) {
    setStatus(E.wStatus, 'ok', (type === 'spawn_where' ? 'Asked for ' : 'Asked to stop ') + count + ' bots: ' + describeFilter(filter) + '.');
    kick('world');
  } else {
    setStatus(E.wStatus, 'err', r.error);
  }
}
function askFilter(type) {
  const n = Math.floor(Number(E.fCount.value));
  if (!(n >= 1 && n <= 1000)) { setStatus(E.wStatus, 'err', 'Enter a count between 1 and 1000.'); return; }
  lsSet('pb_pop_count', String(n));
  sendFilter(type, n, filterOf());
}
function quickCount() {
  const n = Math.floor(Number(E.fCount.value));
  return n >= 1 && n <= 1000 ? n : 3;
}

function renderWorld() {
  const w = P.world;
  if (!w) return;
  const t = w.totals || {};
  const srv = w.server || {};
  // tiles
  const tiles = [
    tile('Players seen', fmtCount(t.players_seen), 'in view of a bot, last ' + fmtDur(w.ttl_secs)),
    tile('Bots online', fmtCount(t.bots_online), 'of ' + fmtCount(t.bots_total)),
  ];
  if (t.players_server != null) {
    tiles.push(tile('Players on server', fmtCount(t.players_server),
      'unlocated: ' + fmtCount(t.players_unlocated) + (srv.online_bots == null ? ' (approx.)' : '')));
    tiles.push(tile('Coverage', num(t.coverage_pct) + '%', 'of them seen by a bot'));
  } else {
    tiles.push(tile('Players on server', '?', srv.configured ? 'no answer yet' : 'no server API'));
    tiles.push(tile('Coverage', '?', 'unknown'));
  }
  tiles.push(tile('Watched', num(t.watched_km2).toFixed(2) + ' km²', 'a bot sees about 128 m'));
  E.wTiles.replaceChildren(...tiles);

  // continent x side
  const cell = {};
  for (const c of w.cells || []) cell[c.continent + '/' + c.alliance] = c;
  const conts = CONTINENTS.filter((c) => c === 'west' || c === 'east' || SIDES.concat(['other', 'unknown']).some((a) => cell[c + '/' + a]));
  const tb = mkTable('grid', [{ t: 'Continent' }, { t: 'Nuia', n: true, title: 'players seen / bots online' },
    { t: 'Haranya', n: true, title: 'players seen / bots online' }, { t: 'Other', n: true, title: 'unplaced or other factions' }]);
  const body = tb.tBodies[0];
  const count = quickCount();
  for (const c of conts) {
    const cells = [{ t: CONT_LABEL[c] }];
    for (const a of SIDES) {
      const x = cell[c + '/' + a] || {};
      const box = h('span', 'pcell');
      box.append(h('span', 'pl', String(num(x.players_seen))), ' / ', h('span', 'bt', String(num(x.bots_online))));
      if (HOME[a] === c) {
        box.append(btn('+', 'Start ' + count + ' ' + SIDE_LABEL[a] + ' bots',
          () => sendFilter('spawn_where', quickCount(), { alliance: a }), 'tiny'));
      }
      if (num(x.bots_online) > 0) {
        box.append(btn('−', 'Stop up to ' + count + ' ' + SIDE_LABEL[a] + ' bots on the ' + CONT_LABEL[c] + ' continent',
          () => sendFilter('despawn_where', quickCount(), { alliance: a, continent: c }), 'tiny'));
      }
      cells.push({ el: box, n: true });
    }
    const rest = ['other', 'unknown'].reduce((acc, a) => {
      const x = cell[c + '/' + a];
      return x ? [acc[0] + num(x.players_seen), acc[1] + num(x.bots_online)] : acc;
    }, [0, 0]);
    cells.push({ t: rest[0] + ' / ' + rest[1], n: true });
    body.append(row(cells));
  }
  const legend = h('div', 'muted pop-note', 'Each cell: players seen / bots online. A "+" starts bots of that side, a "−" stops idle ones.');
  E.wGrid.replaceChildren(tb, legend);

  // zones
  const zones = (w.zones || []).slice(0, 8);
  if (!zones.length) E.wZones.replaceChildren(empty('Nobody anywhere yet.'));
  else {
    const zt = mkTable('zones', [{ t: 'Zone' }, { t: 'Continent' }, { t: 'Players', n: true }, { t: 'Bots', n: true }]);
    for (const z of zones) {
      zt.tBodies[0].append(row([{ t: z.name || ('zone ' + z.zone_key) }, { t: CONT_LABEL[z.continent] || '' },
        { t: num(z.players_seen), n: true }, { t: num(z.bots_online), n: true }]));
    }
    E.wZones.replaceChildren(zt);
  }

  // players
  const players = (w.players || []).slice(0, 12);
  if (!players.length) E.wPlayers.replaceChildren(empty('No real player in view of a bot.'));
  else {
    const pt = mkTable('players', [{ t: 'Name' }, { t: 'Lv', n: true }, { t: 'Side' }, { t: 'Zone' }, { t: 'Seen', n: true }]);
    for (const p of players) {
      pt.tBodies[0].append(row([{ t: p.name || '?' }, { t: p.level, n: true }, { el: sideSpan(p.alliance) },
        { t: p.zone || CONT_LABEL[p.continent] || '' }, { t: fmtAge(p.age_secs) + ' ago', n: true }]));
    }
    E.wPlayers.replaceChildren(pt);
  }

  renderMix();

  // server
  const kv = h('dl', 'kv');
  const add = (k, v) => { kv.append(h('dt', null, k), h('dd', null, v)); };
  if (!srv.configured) {
    E.wServer.replaceChildren(empty('The game server API is not configured ([population] server_api). Player counts come from the bots\' own view only.'));
  } else {
    add('Reachable', srv.reachable ? 'yes, ' + num(srv.latency_ms) + ' ms' : 'no');
    if (srv.checked_ms) add('Checked', fmtClock(srv.checked_ms));
    if (srv.online != null) add('Characters online', fmtCount(srv.online) + (srv.online_bots != null ? ' (' + fmtCount(srv.online_bots) + ' ours)' : ''));
    if (srv.auction_lots != null) add('Auction lots', fmtCount(srv.auction_lots));
    for (const r of srv.status || []) add(r[0], r[1]);
    if (srv.error) add('Last problem', srv.error);
    E.wServer.replaceChildren(kv);
  }
}

function renderMix() {
  const counts = new Map();
  let total = 0;
  for (const b of host.bots()) {
    if (b.activity === 'offline' || b.activity === 'connecting') continue;
    const g = b.goal || 'auto';
    counts.set(g, (counts.get(g) || 0) + 1);
    total++;
  }
  if (!total) { E.wMix.replaceChildren(empty('No bot is in the world.')); return; }
  const rows = [...counts].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1));
  const mt = mkTable('mix', [{ t: 'Goal' }, { t: '' }, { t: 'Bots', n: true }]);
  for (const [g, n] of rows) {
    mt.tBodies[0].append(row([{ t: label(g) }, { el: bar(n / rows[0][1]) },
      { t: n + ' (' + Math.round((100 * n) / total) + '%)', n: true }]));
  }
  E.wMix.replaceChildren(mt);
}

// ------------------------------------------------------------------ the Economy view
function buildEconomy() {
  const root = $('economy');
  E.eSources = h('div', 'muted pop-note');
  E.eStatus = h('div', 'status');
  E.eStatus.setAttribute('role', 'status');
  E.eTable = h('div', 'pop-block');
  E.eWishes = h('div', 'pop-block');

  const form = h('div', 'pop-form');
  E.eTemplate = numInput(1, 4294967295, 1, null);
  E.eTemplate.placeholder = 'Item id';
  E.eTemplate.id = 'e-template';
  E.eTemplate.setAttribute('aria-label', 'Item template id');
  E.eHours = numInput(0, 720, 1, lsGet('pb_wish_hours') || '24');
  E.eHours.setAttribute('aria-label', 'Hours the wish lasts (0 = until removed)');
  E.eHours.title = 'Hours the wish lasts (0 = until removed)';
  form.append(E.eTemplate, E.eHours, btn('Add wish', 'The farming goal will try to grow this item', () => {
    const t = Math.floor(Number(E.eTemplate.value));
    if (!(t >= 1)) { setStatus(E.eStatus, 'err', 'Enter an item id.'); return; }
    lsSet('pb_wish_hours', E.eHours.value);
    addWish(t, Math.floor(Number(E.eHours.value)) || 0);
  }));

  root.append(
    E.eSources, E.eStatus,
    section('Scarce items', '(most scarce first)'), E.eTable,
    section('Wishes', '(items the bots should grow)'), form, E.eWishes,
  );
}

function wishRequests(skip) {
  const d = P.director;
  if (!d) return [];
  const now = Date.now();
  return (d.policy.boosts || []).filter((b) => b.template !== skip).map((b) => ({
    template: b.template,
    hours: b.until_ms ? Math.max(1, Math.ceil((b.until_ms - now) / 3600000)) : 0,
  }));
}
async function sendPolicy(patch, statusEl, okText) {
  const r = await postJson('/api/director/policy', patch);
  if (r.ok) setStatus(statusEl, 'ok', okText);
  else setStatus(statusEl, 'err', r.error);
  kick('director');
  return r.ok;
}
async function addWish(template, hours) {
  const list = wishRequests(template);
  if (list.length >= 16) { setStatus(E.eStatus, 'err', 'At most 16 wishes.'); return; }
  list.push({ template, hours: Math.max(0, Math.min(720, hours)) });
  if (await sendPolicy({ boosts: list }, E.eStatus, 'Wished item ' + template + '.')) kick('economy');
}
async function dropWish(template) {
  if (await sendPolicy({ boosts: wishRequests(template) }, E.eStatus, 'Removed the wish for item ' + template + '.')) kick('economy');
}

function renderEconomy() {
  const e = P.economy;
  if (!e) return;
  const s = e.sources || {};
  setText(E.eSources, 'From ' + num(s.bot_notes) + ' price notes of the bots, '
    + num(s.chat_lines) + ' trade chat lines in the last ' + fmtDur(s.chat_window_secs)
    + (s.server_lots != null ? ', and the server\'s ' + fmtCount(s.server_lots) + ' auction lots.' : '. No server auction list.'));
  const items = e.items || [];
  if (!items.length) E.eTable.replaceChildren(empty('Nothing priced yet. Bots look up prices at the auction house and listen to trade chat.'));
  else {
    const tb = mkTable('items', [{ t: 'Item' }, { t: 'Scarcity' }, { t: 'Lots', n: true, title: 'on the auction house' },
      { t: 'Price', n: true, title: 'market price per item and how many times the vendor price' },
      { t: '30 min', n: true, title: 'price change' }, { t: 'Chat', n: true, title: 'want to buy / want to sell lines' }, { t: '' }]);
    for (const it of items) {
      const sc = num(it.scarcity);
      const price = it.unit_price ? fmtMoney(it.unit_price) + (it.price_ratio ? ' (' + it.price_ratio.toFixed(1) + 'x)' : '') : '-';
      const trend = it.trend_pct ? (it.trend_pct > 0 ? '+' : '') + it.trend_pct.toFixed(0) + '%' : '';
      const b = btn(it.boosted ? 'Drop' : 'Grow',
        it.boosted ? 'Remove the wish for this item' : 'Wish for more of this item', () => (it.boosted ? dropWish(it.template) : addWish(it.template, 24)), 'tiny');
      const name = h('span', it.boosted ? 'boosted' : null, it.name || ('item ' + it.template));
      const tr = row([
        { el: name, title: '#' + it.template }, { el: bar(sc, sc >= 0.7 ? 'hot' : sc >= 0.4 ? 'warm' : ''), title: (it.reasons || []).join(', ') || 'plenty' },
        { t: (it.supply_source === 'server' ? '' : '~') + num(it.lots), n: true, title: it.supply_source === 'server' ? 'counted by the server' : 'seen by the bots (they look at five at most)' },
        { t: price, n: true }, { t: trend, n: true }, { t: it.wtb || it.wts ? it.wtb + ' / ' + it.wts : '', n: true }, { el: b },
      ]);
      tr.title = (it.reasons || []).join(' · ');
      tb.tBodies[0].append(tr);
    }
    E.eTable.replaceChildren(tb);
  }
  renderWishes();
}

function renderWishes() {
  const d = P.director;
  const names = new Map();
  for (const it of (P.economy && P.economy.items) || []) names.set(it.template, it.name);
  if (!d) { E.eWishes.replaceChildren(empty('The director is not available.')); return; }
  const rows = [];
  const now = Date.now();
  for (const b of d.policy.boosts || []) {
    const left = b.until_ms ? 'for ' + fmtDur(Math.max(0, (b.until_ms - now) / 1000)) : 'until removed';
    rows.push([b.template, left, false]);
  }
  for (const t of d.auto_wishes || []) rows.push([t, 'automatic', true]);
  if (!rows.length) { E.eWishes.replaceChildren(empty('No wishes. The farming goal grows what is wished and what is scarce.')); return; }
  const ul = h('ul', 'plain');
  for (const [t, left, auto] of rows) {
    const li = h('li');
    const a = h('span', null, (names.get(t) || 'Item') + ' #' + t);
    const b = h('span', null, left + ' ');
    if (!auto) b.append(btn('×', 'Remove the wish', () => dropWish(t), 'tiny'));
    li.append(a, b);
    ul.append(li);
  }
  E.eWishes.replaceChildren(ul);
}

// ------------------------------------------------------------------ the Director view
function buildDirector() {
  const root = $('director');
  E.dHead = h('div', 'dir-head');
  E.dMode = h('span', 'pill mode-off', 'Off');
  E.dKill = btn('Switch off', 'Kill switch: the director does nothing at all while it is off', () => setActive(false), 'danger');
  E.dOn = btn('Switch on', 'Let the director act (as a dry run unless live is chosen)', () => setActive(true));
  E.dDry = h('input');
  E.dDry.type = 'checkbox';
  E.dDry.id = 'd-dry';
  const dryLabel = h('label', 'dry');
  dryLabel.title = 'On: the director only writes down what it would do. Off: it starts and stops bots by itself.';
  dryLabel.append(E.dDry, ' Dry run');
  E.dDry.addEventListener('change', onDry);
  E.dHead.append(E.dMode, E.dOn, E.dKill, dryLabel);
  E.dStatus = h('div', 'status');
  E.dStatus.setAttribute('role', 'status');
  E.dNote = h('div', 'muted pop-note');
  E.dSides = h('div', 'pop-block');
  E.dHealth = h('div', 'pop-block');

  // policy form
  E.dForm = h('div', 'pop-policy');
  E.dInputs = {};
  for (const [key, text, min, max, stepv, hint] of POLICY_NUMBERS) {
    const lab = h('label', null);
    lab.title = hint;
    lab.append(h('span', null, text));
    const inp = numInput(min, max, stepv, null);
    inp.setAttribute('aria-label', text);
    inp.addEventListener('input', () => { inp.dataset.dirty = '1'; });
    E.dInputs[key] = inp;
    lab.append(inp);
    E.dForm.append(lab);
  }
  const despawnLabel = h('label', 'check');
  despawnLabel.title = 'The director may stop the bots it started when a side has too many.';
  E.dDespawn = h('input');
  E.dDespawn.type = 'checkbox';
  E.dDespawn.addEventListener('change', () => { E.dDespawn.dataset.dirty = '1'; });
  despawnLabel.append(E.dDespawn, ' May stop its own bots');
  E.dForm.append(despawnLabel);

  // goal bias
  E.dBias = h('div', 'pop-policy bias');
  E.dBiasInputs = {};
  for (const g of BIAS_GOALS) {
    const lab = h('label', null);
    lab.append(h('span', null, label(g)));
    const inp = numInput(0.1, 3, 0.1, null);
    inp.placeholder = '1';
    inp.setAttribute('aria-label', 'Weight of ' + g);
    inp.addEventListener('input', () => { inp.dataset.dirty = '1'; });
    E.dBiasInputs[g] = inp;
    lab.append(inp);
    E.dBias.append(lab);
  }
  const apply = h('div', 'cmd-row');
  apply.append(btn('Apply', 'Send the changed numbers', applyPolicy), btn('Revert', 'Show the current policy again', () => {
    for (const i of Object.values(E.dInputs).concat(Object.values(E.dBiasInputs), [E.dDespawn])) delete i.dataset.dirty;
    renderDirector(true);
  }));
  E.dLog = h('div', 'pop-block');

  root.append(
    E.dHead, E.dStatus, E.dNote,
    section('Sides', '(characters = players + bots)'), E.dSides,
    section('Process health'), E.dHealth,
    section('Policy'), E.dForm,
    section('Goal weights', '(multipliers on what the bots choose to do; blank = 1)'), E.dBias, apply,
    section('Log', '(newest first)'), E.dLog,
  );
}

async function setActive(on) {
  const d = P.director;
  if (on && d && !d.policy.dry_run && !confirm('The director is set to LIVE: it will start and stop bots by itself.\n\nSwitch it on?')) return;
  await sendPolicy({ active: on }, E.dStatus, on ? 'Director switched on.' : 'Director switched off.');
}
async function onDry() {
  const dry = E.dDry.checked;
  if (!dry && !confirm('Go live? The director will start and stop bots by itself, within the caps below.\n\nYou can switch it off at any time.')) {
    E.dDry.checked = true;
    return;
  }
  await sendPolicy({ dry_run: dry }, E.dStatus, dry ? 'Dry run: the director only writes down what it would do.' : 'The director is live.');
}
async function applyPolicy() {
  const d = P.director;
  if (!d) return;
  const patch = {};
  for (const [key, , min, max] of POLICY_NUMBERS) {
    const inp = E.dInputs[key];
    if (inp.dataset.dirty !== '1') continue;
    const v = Number(inp.value);
    if (inp.value === '' || !isFinite(v) || v < min || v > max) { setStatus(E.dStatus, 'err', 'Check the number for "' + inp.getAttribute('aria-label') + '" (' + min + ' to ' + max + ').'); return; }
    patch[key] = key === 'auto_boost_min' ? v : Math.round(v);
  }
  if (E.dDespawn.dataset.dirty === '1') patch.despawn = E.dDespawn.checked;
  if (Object.values(E.dBiasInputs).some((i) => i.dataset.dirty === '1')) {
    const bias = {};
    for (const g of BIAS_GOALS) {
      const inp = E.dBiasInputs[g];
      if (inp.value === '') continue;
      const v = Number(inp.value);
      if (!isFinite(v) || v < 0.1 || v > 3) { setStatus(E.dStatus, 'err', 'Goal weights go from 0.1 to 3.'); return; }
      if (v !== 1) bias[g] = v;
    }
    patch.goal_bias = bias;
  }
  if (!Object.keys(patch).length) { setStatus(E.dStatus, '', 'Nothing changed.'); return; }
  if (await sendPolicy(patch, E.dStatus, 'Policy applied.')) {
    for (const i of Object.values(E.dInputs).concat(Object.values(E.dBiasInputs), [E.dDespawn])) delete i.dataset.dirty;
  }
}

function renderBadge() {
  const d = P.director;
  const b = $('tab-mode');
  if (!b) return;
  const mode = d ? d.mode : 'off';
  const show = d && d.available && mode !== 'off';
  b.hidden = !show;
  if (show) {
    b.className = 'badge mode-' + mode;
    setText(b, '');
    b.title = MODE_LABEL[mode] || mode;
  }
}

function renderDirector(force) {
  const d = P.director;
  if (!d) return;
  const p = d.policy;
  E.dMode.className = 'pill mode-' + d.mode;
  setText(E.dMode, d.available ? (MODE_LABEL[d.mode] || d.mode) : 'Disabled');
  E.dKill.hidden = !p.active;
  E.dOn.hidden = p.active;
  E.dOn.disabled = !d.available;
  E.dDry.checked = !!p.dry_run;
  E.dDry.disabled = !d.available;
  setText(E.dNote, d.available
    ? (p.active
      ? (p.dry_run ? 'Dry run: nothing is started or stopped; the log shows what would be done.' : 'Live: the director starts and stops bots by itself, within the caps.')
      : 'Switched off: the director does nothing.')
    : 'The director is switched off in the configuration ([population] enabled = false). Its numbers are shown but it cannot be switched on.');

  // sides
  const st = mkTable('sides', [{ t: 'Side' }, { t: 'Players', n: true }, { t: 'Bots', n: true, title: 'in the world or logging in' },
    { t: 'Its own', n: true, title: 'started by the director' }, { t: 'Target', n: true }, { t: '' }]);
  for (const s of d.sides || []) {
    const tgt = Math.max(num(s.target), num(s.bots), 1);
    st.tBodies[0].append(row([{ el: sideSpan(s.alliance) }, { t: num(s.players), n: true }, { t: num(s.bots), n: true },
      { t: num(s.owned), n: true }, { t: num(s.target), n: true }, { el: bar(num(s.bots) / tgt) }]));
  }
  const pace = h('div', 'muted pop-note',
    'Started in the last minute: ' + num(d.spawned_last_min) + ' of at most ' + num(d.max_spawn_per_min)
    + ' · at least ' + num(d.min_action_gap_secs) + ' s between actions'
    + (p.active && d.next_action_secs ? ' · next action in ' + num(d.next_action_secs) + ' s' : '')
    + ' · ' + num(d.owned) + ' bots are the director\'s');
  E.dSides.replaceChildren(st, pace);

  // health
  const hl = d.health || {};
  const kv = h('dl', 'kv');
  const add = (k, v) => { kv.append(h('dt', null, k), h('dd', null, v)); };
  add('Memory', fmtBytes(num(hl.rss_mb) * 1048576) + (p.backoff_rss_mb ? ' (limit ' + p.backoff_rss_mb + ' MB)' : ''));
  add('Packets in/s', fmtCount(hl.packets_in_per_sec) + (p.backoff_packets_in ? ' (limit ' + fmtCount(p.backoff_packets_in) + ')' : ''));
  add('Packets out/s', fmtCount(hl.packets_out_per_sec));
  add('Failed sessions', num(hl.login_failures) + ' in 5 min' + (p.backoff_login_failures ? ' (limit ' + p.backoff_login_failures + ')' : ''));
  add('Open server issues', num(hl.open_issues) + (p.backoff_open_issues ? ' (limit ' + p.backoff_open_issues + ')' : ''));
  add('Logging in', num(hl.connecting));
  if (hl.holding) add('On hold', hl.holding);
  E.dHealth.replaceChildren(kv);

  // forms: do not overwrite what the operator is typing
  for (const [key] of POLICY_NUMBERS) {
    const inp = E.dInputs[key];
    if (force || (inp.dataset.dirty !== '1' && document.activeElement !== inp)) inp.value = String(p[key]);
  }
  if (force || (E.dDespawn.dataset.dirty !== '1' && document.activeElement !== E.dDespawn)) E.dDespawn.checked = !!p.despawn;
  for (const g of BIAS_GOALS) {
    const inp = E.dBiasInputs[g];
    if (force || (inp.dataset.dirty !== '1' && document.activeElement !== inp)) {
      const v = p.goal_bias && p.goal_bias[g];
      inp.value = v == null ? '' : String(v);
    }
  }

  // log
  const log = (d.log || []).slice(0, 100);
  if (!log.length) E.dLog.replaceChildren(empty('Nothing yet.'));
  else {
    const lt = mkTable('log', [{ t: 'Time' }, { t: 'What' }, { t: 'Why' }]);
    for (const r of log) {
      const what = (KIND_LABEL[r.kind] || r.kind) + (r.count ? ' ' + r.count : '') + (r.alliance && r.alliance !== 'unknown' ? ' ' + (SIDE_LABEL[r.alliance] || r.alliance) : '');
      const why = h('div');
      why.append(h('div', null, r.reason || ''));
      if (r.result) why.append(h('div', 'muted', r.result));
      const tr = row([{ t: fmtClock(r.at_ms) }, { t: what + (r.applied || r.kind === 'policy' || r.kind === 'result' || r.kind === 'wish' ? '' : ' (not done)') }, { el: why }]);
      if (!r.applied && (r.kind === 'spawn' || r.kind === 'despawn')) tr.className = 'dim';
      lt.tBodies[0].append(tr);
    }
    E.dLog.replaceChildren(lt);
  }
}

// ------------------------------------------------------------------ the heat overlay
function drawHeat(ctx, v) {
  if (!P.heat || !P.world || !(P.world.heat || []).length) return;
  const { W, H, scale, cx, cy } = v;
  const size = HEAT_CELL * scale;
  const layers = [['bots', '47,109,246'], ['players', '229,56,59']];
  ctx.save();
  for (const [key, rgb] of layers) {
    for (const c of P.world.heat) {
      const n = num(c[key]);
      if (!n) continue;
      const x = (c.x - cx) * scale + W / 2, y = (cy - c.y) * scale + H / 2;
      if (x + size < 0 || x - size > W || y + size < 0 || y - size > H) continue;
      const a = Math.min(0.7, 0.2 + 0.1 * Math.sqrt(n));
      ctx.fillStyle = 'rgba(' + rgb + ',' + a.toFixed(2) + ')';
      ctx.fillRect(x - size / 2, y - size / 2, size, size);
      if (key === 'players' && size >= 22) {
        ctx.fillStyle = '#ffffff';
        ctx.font = '600 11px system-ui, sans-serif';
        ctx.textAlign = 'center';
        ctx.textBaseline = 'middle';
        ctx.fillText(String(n), x, y);
      }
    }
  }
  ctx.font = '11px system-ui, sans-serif';
  ctx.textAlign = 'left';
  ctx.textBaseline = 'bottom';
  ctx.fillStyle = v.colors.label;
  ctx.fillText('Heat: red = players, blue = bots (512 m squares)', 8, H - 8);
  ctx.restore();
}

// ------------------------------------------------------------------ boot
function onTab(tab) {
  P.tab = tab;
  if (tab === 'world') { renderWorld(); kick('world'); }
  else if (tab === 'economy') { renderEconomy(); kick('economy'); kick('director'); }
  else if (tab === 'director') { renderDirector(); kick('director'); }
}

function init() {
  for (const id of ['world', 'economy', 'director']) if (!$(id)) return;
  buildWorld();
  buildEconomy();
  buildDirector();
  const heat = $('o-heat');
  if (heat) {
    heat.checked = P.heat;
    heat.addEventListener('change', () => {
      P.heat = heat.checked;
      lsSet('pb_heat', P.heat ? '1' : '0');
      if (P.heat) kick('world');
      host.requestDraw();
    });
  }
  host.drawHeat = drawHeat;
  host.tabChanged = onTab;
  P.tab = host.currentTab();
  onTab(P.tab);
  setInterval(step, 500);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') { P.last.world = P.last.economy = P.last.director = -Infinity; step(); }
  });
  step();
}

init();
})();
