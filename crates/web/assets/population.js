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
const SIDE_LABEL = { nuia: '努伊亚', haranya: '哈拉尼亚', other: '其他', unknown: '?' };
const CONTINENTS = ['west', 'east', 'auroria', 'sea', 'unknown'];
const CONT_LABEL = { west: '西部', east: '东部', auroria: '奥罗瑞亚', sea: '海域', unknown: '未知' };
const HOME = { nuia: 'west', haranya: 'east' };
const RACES = [['nuian', '努伊安'], ['elf', '精灵'], ['hariharan', '哈里兰'], ['ferre', '兽灵']];
const POLL = { world: 3000, economy: 5000, directorFast: 2000, directorSlow: 5000 };
const HEAT_CELL = 512;
const MODE_LABEL = { off: '关闭', dry_run: '演练', live: '运行中', holding: '暂停' };
const KIND_LABEL = {
  spawn: '启动机器人', despawn: '停止机器人', hold: '暂缓', policy: '策略', wish: '愿望', result: '完成',
};
// numeric policy fields: [key, label, min, max, step, hint]
const POLICY_NUMBERS = [
  ['max_online', '同时在线上限', 0, 5000, 1, '所有机器人合计；超过此数调度器不再启动。'],
  ['min_per_side', '每阵营最少', 0, 5000, 1, '每个阵营至少保留这么多机器人。'],
  ['per_side_cap', '每阵营上限', 0, 5000, 1, '调度器不会让任一阵营超过此数。'],
  ['mirror_pct', '镜像比例 %', 0, 1000, 5, '每个阵营按对方每 100 名玩家补足这么多角色。'],
  ['step', '单次操作数量', 1, 1000, 1, '一次操作最多启动或停止的机器人数。'],
  ['hysteresis', '目标上浮保留', 0, 1000, 1, '超出目标多少以内不主动裁减。'],
  ['backoff_rss_mb', '内存上限 MB', 0, 1000000, 64, '进程内存超过此值后不再启动（0 = 忽略）。'],
  ['backoff_packets_in', '收包上限 /秒', 0, 100000000, 500, '每秒收包数超过此值后不再启动（0 = 忽略）。'],
  ['backoff_login_failures', '登录失败上限', 0, 5000, 1, '五分钟内失败或过短的会话数（0 = 忽略）。'],
  ['backoff_open_issues', '问题数上限', 0, 100000, 1, '未解决的服务器问题数（0 = 忽略）。'],
  ['auto_boost', '自动愿望数', 0, 16, 1, '最稀缺的物品会自动成为愿望（0 = 不自动）。'],
  ['auto_boost_min', '稀缺阈值', 0, 1, 0.05, '只对稀缺度达到此值的物品设愿（0 到 1）。'],
];
const BIAS_GOALS = ['quest', 'grind', 'duel', 'trade', 'trade_run', 'housing', 'dungeon', 'sail', 'auction',
  'market', 'guild', 'gank', 'warzone', 'event', 'companion', 'farm', 'rest', 'wander'];

// ------------------------------------------------------------------ state
const P = {
  tab: 'bots', world: null, economy: null, director: null,
  last: { world: -Infinity, economy: -Infinity, director: -Infinity },
  busy: { world: false, economy: false, director: false },
  heat: lsGet('pb_heat') === '1' || /[#&]heat=1(?:&|$)/.test(location.hash),
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
    if (P.tab === 'world') setStatus(E.wStatus, 'err', '无法加载世界视图：' + ((e && e.message) || e));
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
    if (P.tab === 'economy') setStatus(E.eStatus, 'err', '无法加载经济视图：' + ((e && e.message) || e));
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
    if (P.tab === 'director') setStatus(E.dStatus, 'err', '无法加载调度器：' + ((e && e.message) || e));
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
  E.fCount.setAttribute('aria-label', '机器人数量');
  E.fSide = sel([['', '任意阵营'], ['nuia', '努伊亚'], ['haranya', '哈拉尼亚']]);
  E.fSide.setAttribute('aria-label', '阵营');
  E.fRace = sel([['', '任意种族']].concat(RACES));
  E.fRace.setAttribute('aria-label', '种族');
  E.fCont = sel([['', '任意大陆'], ['west', '西部'], ['east', '东部'], ['auroria', '奥罗瑞亚'], ['sea', '海域']]);
  E.fCont.setAttribute('aria-label', '大陆');
  E.fCont.title = '生成：该种族出生的大陆。移除：机器人当前所在的大陆。';
  form.append(E.fCount, E.fSide, E.fRace, E.fCont,
    btn('生成', '登录符合筛选条件的机器人', () => askFilter('spawn_where')),
    btn('移除', '登出符合筛选条件的机器人（优先空闲的；交易、决斗或副本中的不会移除）', () => askFilter('despawn_where')));
  E.fCount.addEventListener('change', () => lsSet('pb_pop_count', E.fCount.value));

  root.append(
    E.wTiles,
    section('人员分布', '（玩家指机器人视野内的真人）'), E.wGrid,
    section('启动或停止机器人'), form, E.wStatus,
    section('最繁忙的区域'), E.wZones,
    section('视野内的玩家'), E.wPlayers,
    section('机器人都在做什么'), E.wMix,
    section('游戏服务器'), E.wServer,
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
  if (f.continent) bits.push(CONT_LABEL[f.continent] + '大陆');
  return bits.length ? bits.join(' ') : '不限';
}
async function sendFilter(type, count, filter) {
  const r = await postJson('/api/command', { type, count, filter });
  if (r.ok) {
    setStatus(E.wStatus, 'ok', (type === 'spawn_where' ? '已请求启动 ' : '已请求停止 ') + count + ' 个机器人：' + describeFilter(filter) + '。');
    kick('world');
  } else {
    setStatus(E.wStatus, 'err', r.error);
  }
}
function askFilter(type) {
  const n = Math.floor(Number(E.fCount.value));
  if (!(n >= 1 && n <= 1000)) { setStatus(E.wStatus, 'err', '请输入 1 到 1000 之间的数量。'); return; }
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
    tile('可见玩家', fmtCount(t.players_seen), '机器人视野内，最近 ' + fmtDur(w.ttl_secs)),
    tile('在线机器人', fmtCount(t.bots_online), '共 ' + fmtCount(t.bots_total)),
  ];
  if (t.players_server != null) {
    tiles.push(tile('服务器玩家', fmtCount(t.players_server),
      '未定位：' + fmtCount(t.players_unlocated) + (srv.online_bots == null ? '（约数）' : '')));
    tiles.push(tile('覆盖率', num(t.coverage_pct) + '%', '其中被机器人看到的比例'));
  } else {
    tiles.push(tile('服务器玩家', '?', srv.configured ? '尚无应答' : '未配置服务器 API'));
    tiles.push(tile('覆盖率', '?', '未知'));
  }
  tiles.push(tile('监视范围', num(t.watched_km2).toFixed(2) + ' km²', '每个机器人约可见 128 米'));
  E.wTiles.replaceChildren(...tiles);

  // continent x side
  const cell = {};
  for (const c of w.cells || []) cell[c.continent + '/' + c.alliance] = c;
  const conts = CONTINENTS.filter((c) => c === 'west' || c === 'east' || SIDES.concat(['other', 'unknown']).some((a) => cell[c + '/' + a]));
  const tb = mkTable('grid', [{ t: '大陆' }, { t: '努伊亚', n: true, title: '可见玩家 / 在线机器人' },
    { t: '哈拉尼亚', n: true, title: '可见玩家 / 在线机器人' }, { t: '其他', n: true, title: '未定位或其他阵营' }]);
  const body = tb.tBodies[0];
  const count = quickCount();
  for (const c of conts) {
    const cells = [{ t: CONT_LABEL[c] }];
    for (const a of SIDES) {
      const x = cell[c + '/' + a] || {};
      const box = h('span', 'pcell');
      box.append(h('span', 'pl', String(num(x.players_seen))), ' / ', h('span', 'bt', String(num(x.bots_online))));
      if (HOME[a] === c) {
        box.append(btn('+', '启动 ' + count + ' 个' + SIDE_LABEL[a] + '机器人',
          () => sendFilter('spawn_where', quickCount(), { alliance: a }), 'tiny'));
      }
      if (num(x.bots_online) > 0) {
        box.append(btn('−', '最多停止 ' + count + ' 个' + SIDE_LABEL[a] + '机器人（' + CONT_LABEL[c] + '大陆）',
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
  const legend = h('div', 'muted pop-note', '每格：可见玩家 / 在线机器人。"+" 启动该阵营的机器人，"−" 停止其中的空闲机器人。');
  E.wGrid.replaceChildren(tb, legend);

  // zones
  const zones = (w.zones || []).slice(0, 8);
  if (!zones.length) E.wZones.replaceChildren(empty('目前还没有人在任何地方。'));
  else {
    const zt = mkTable('zones', [{ t: '区域' }, { t: '大陆' }, { t: '玩家', n: true }, { t: '机器人', n: true }]);
    for (const z of zones) {
      zt.tBodies[0].append(row([{ t: z.name || ('区域 ' + z.zone_key) }, { t: CONT_LABEL[z.continent] || '' },
        { t: num(z.players_seen), n: true }, { t: num(z.bots_online), n: true }]));
    }
    E.wZones.replaceChildren(zt);
  }

  // players
  const players = (w.players || []).slice(0, 12);
  if (!players.length) E.wPlayers.replaceChildren(empty('没有机器人的视野内有真人玩家。'));
  else {
    const pt = mkTable('players', [{ t: '名字' }, { t: '等级', n: true }, { t: '阵营' }, { t: '区域' }, { t: '看到于', n: true }]);
    for (const p of players) {
      pt.tBodies[0].append(row([{ t: p.name || '?' }, { t: p.level, n: true }, { el: sideSpan(p.alliance) },
        { t: p.zone || CONT_LABEL[p.continent] || '' }, { t: fmtAge(p.age_secs) + '前', n: true }]));
    }
    E.wPlayers.replaceChildren(pt);
  }

  renderMix();

  // server
  const kv = h('dl', 'kv');
  const add = (k, v) => { kv.append(h('dt', null, k), h('dd', null, v)); };
  if (!srv.configured) {
    E.wServer.replaceChildren(empty('未配置游戏服务器 API（[population] server_api）。玩家数仅来自机器人自身的视野。'));
  } else {
    add('可连通', srv.reachable ? '是，' + num(srv.latency_ms) + ' 毫秒' : '否');
    if (srv.checked_ms) add('检查于', fmtClock(srv.checked_ms));
    if (srv.online != null) add('在线角色', fmtCount(srv.online) + (srv.online_bots != null ? '（其中我们 ' + fmtCount(srv.online_bots) + ' 个）' : ''));
    if (srv.auction_lots != null) add('拍卖挂单', fmtCount(srv.auction_lots));
    for (const r of srv.status || []) add(r[0], r[1]);
    if (srv.error) add('最近的问题', srv.error);
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
  if (!total) { E.wMix.replaceChildren(empty('没有机器人在世界中。')); return; }
  const rows = [...counts].sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1));
  const mt = mkTable('mix', [{ t: '目标' }, { t: '' }, { t: '机器人', n: true }]);
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
  E.eTemplate.placeholder = '物品 ID';
  E.eTemplate.id = 'e-template';
  E.eTemplate.setAttribute('aria-label', '物品模板 ID');
  E.eHours = numInput(0, 720, 1, lsGet('pb_wish_hours') || '24');
  E.eHours.setAttribute('aria-label', '愿望持续小时数（0 = 直到移除）');
  E.eHours.title = '愿望持续小时数（0 = 直到移除）';
  form.append(E.eTemplate, E.eHours, btn('添加愿望', '种植目标会尝试培育该物品', () => {
    const t = Math.floor(Number(E.eTemplate.value));
    if (!(t >= 1)) { setStatus(E.eStatus, 'err', '请输入物品 ID。'); return; }
    lsSet('pb_wish_hours', E.eHours.value);
    addWish(t, Math.floor(Number(E.eHours.value)) || 0);
  }));

  root.append(
    E.eSources, E.eStatus,
    section('稀缺物品', '（最稀缺的在前）'), E.eTable,
    section('愿望', '（希望机器人培育的物品）'), form, E.eWishes,
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
  if (list.length >= 16) { setStatus(E.eStatus, 'err', '最多 16 个愿望。'); return; }
  list.push({ template, hours: Math.max(0, Math.min(720, hours)) });
  if (await sendPolicy({ boosts: list }, E.eStatus, '已添加愿望：物品 ' + template + '。')) kick('economy');
}
async function dropWish(template) {
  if (await sendPolicy({ boosts: wishRequests(template) }, E.eStatus, '已移除物品 ' + template + ' 的愿望。')) kick('economy');
}

function renderEconomy() {
  const e = P.economy;
  if (!e) return;
  const s = e.sources || {};
  setText(E.eSources, '来自机器人的 ' + num(s.bot_notes) + ' 条价格记录、'
    + num(s.chat_lines) + ' 条贸易聊天（最近 ' + fmtDur(s.chat_window_secs) + '）'
    + (s.server_lots != null ? '，以及服务器上的 ' + fmtCount(s.server_lots) + ' 个拍卖挂单。' : '。没有服务器拍卖列表。'));
  const items = e.items || [];
  if (!items.length) E.eTable.replaceChildren(empty('还没有价格数据。机器人会在拍卖行查价并监听贸易聊天。'));
  else {
    const tb = mkTable('items', [{ t: '物品' }, { t: '稀缺度' }, { t: '挂单', n: true, title: '拍卖行上的挂单数' },
      { t: '价格', n: true, title: '单件市场价，以及是商人收购价的几倍' },
      { t: '30 分钟', n: true, title: '价格变化' }, { t: '聊天', n: true, title: '求购 / 出售 的行数' },
      { t: '培育中', n: true, title: '机器人的作物将产出多少（种植目标）' }, { t: '' }]);
    for (const it of items) {
      const sc = num(it.scarcity);
      const price = it.unit_price ? fmtMoney(it.unit_price) + (it.price_ratio ? ' (' + it.price_ratio.toFixed(1) + 'x)' : '') : '-';
      const trend = it.trend_pct ? (it.trend_pct > 0 ? '+' : '') + it.trend_pct.toFixed(0) + '%' : '';
      const b = btn(it.boosted ? '取消' : '培育',
        it.boosted ? '移除该物品的愿望' : '希望培育更多该物品', () => (it.boosted ? dropWish(it.template) : addWish(it.template, 24)), 'tiny');
      const name = h('span', it.boosted ? 'boosted' : null, it.name || ('物品 ' + it.template));
      const tr = row([
        { el: name, title: '#' + it.template }, { el: bar(sc, sc >= 0.7 ? 'hot' : sc >= 0.4 ? 'warm' : ''), title: (it.reasons || []).join(', ') || '充足' },
        { t: (it.supply_source === 'server' ? '' : '~') + num(it.lots), n: true, title: it.supply_source === 'server' ? '服务器统计' : '机器人所见（最多看五个）' },
        { t: price, n: true }, { t: trend, n: true }, { t: it.wtb || it.wts ? it.wtb + ' / ' + it.wts : '', n: true },
        { t: it.growing ? '+' + Math.round(it.growing) : '', n: true, title: it.growing ? '机器人正在培育' : '' }, { el: b },
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
  if (!d) { E.eWishes.replaceChildren(empty('调度器不可用。')); return; }
  const rows = [];
  const now = Date.now();
  for (const b of d.policy.boosts || []) {
    const left = b.until_ms ? '剩余 ' + fmtDur(Math.max(0, (b.until_ms - now) / 1000)) : '直到移除';
    rows.push([b.template, left, false]);
  }
  for (const t of d.auto_wishes || []) rows.push([t, '自动', true]);
  if (!rows.length) { E.eWishes.replaceChildren(empty('没有愿望。种植目标会培育有愿望的以及稀缺的物品。')); return; }
  const ul = h('ul', 'plain');
  for (const [t, left, auto] of rows) {
    const li = h('li');
    const a = h('span', null, (names.get(t) || '物品') + ' #' + t);
    const b = h('span', null, left + ' ');
    if (!auto) b.append(btn('×', '移除该愿望', () => dropWish(t), 'tiny'));
    li.append(a, b);
    ul.append(li);
  }
  E.eWishes.replaceChildren(ul);
}

// ------------------------------------------------------------------ the Director view
function buildDirector() {
  const root = $('director');
  E.dHead = h('div', 'dir-head');
  E.dMode = h('span', 'pill mode-off', '关闭');
  E.dKill = btn('关闭', '总开关：关闭期间调度器完全不动手', () => setActive(false), 'danger');
  E.dOn = btn('开启', '允许调度器行动（未选实时则只是演练）', () => setActive(true));
  E.dDry = h('input');
  E.dDry.type = 'checkbox';
  E.dDry.id = 'd-dry';
  const dryLabel = h('label', 'dry');
  dryLabel.title = '勾选：调度器只记录打算做什么。取消：它会自行启动和停止机器人。';
  dryLabel.append(E.dDry, ' 演练');
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
  despawnLabel.title = '某阵营人过多时，调度器可以停掉自己启动的机器人。';
  E.dDespawn = h('input');
  E.dDespawn.type = 'checkbox';
  E.dDespawn.addEventListener('change', () => { E.dDespawn.dataset.dirty = '1'; });
  despawnLabel.append(E.dDespawn, ' 可停掉自己启动的');
  E.dForm.append(despawnLabel);

  // goal bias
  E.dBias = h('div', 'pop-policy bias');
  E.dBiasInputs = {};
  for (const g of BIAS_GOALS) {
    const lab = h('label', null);
    lab.append(h('span', null, label(g)));
    const inp = numInput(0.1, 3, 0.1, null);
    inp.placeholder = '1';
    inp.setAttribute('aria-label', g + ' 权重');
    inp.addEventListener('input', () => { inp.dataset.dirty = '1'; });
    E.dBiasInputs[g] = inp;
    lab.append(inp);
    E.dBias.append(lab);
  }
  const apply = h('div', 'cmd-row');
  apply.append(btn('应用', '提交修改过的数值', applyPolicy), btn('还原', '重新显示当前策略', () => {
    for (const i of Object.values(E.dInputs).concat(Object.values(E.dBiasInputs), [E.dDespawn])) delete i.dataset.dirty;
    renderDirector(true);
  }));
  E.dLog = h('div', 'pop-block');

  root.append(
    E.dHead, E.dStatus, E.dNote,
    section('阵营', '（角色 = 玩家 + 机器人）'), E.dSides,
    section('进程健康度'), E.dHealth,
    section('策略'), E.dForm,
    section('目标权重', '（对机器人选择目标的倍数；留空 = 1）'), E.dBias, apply,
    section('日志', '（最新在前）'), E.dLog,
  );
}

async function setActive(on) {
  const d = P.director;
  if (on && d && !d.policy.dry_run && !confirm('调度器已设为「实时」：它会自行启动和停止机器人。\n\n确定开启？')) return;
  await sendPolicy({ active: on }, E.dStatus, on ? '调度器已开启。' : '调度器已关闭。');
}
async function onDry() {
  const dry = E.dDry.checked;
  if (!dry && !confirm('切换到实时？调度器会在下面的上限内自行启动和停止机器人。\n\n随时可以关闭。')) {
    E.dDry.checked = true;
    return;
  }
  await sendPolicy({ dry_run: dry }, E.dStatus, dry ? '演练：调度器只记录打算做什么。' : '调度器已实时运行。');
}
async function applyPolicy() {
  const d = P.director;
  if (!d) return;
  const patch = {};
  for (const [key, , min, max] of POLICY_NUMBERS) {
    const inp = E.dInputs[key];
    if (inp.dataset.dirty !== '1') continue;
    const v = Number(inp.value);
    if (inp.value === '' || !isFinite(v) || v < min || v > max) { setStatus(E.dStatus, 'err', '请检查「' + inp.getAttribute('aria-label') + '」的数值（' + min + ' 到 ' + max + '）。'); return; }
    patch[key] = key === 'auto_boost_min' ? v : Math.round(v);
  }
  if (E.dDespawn.dataset.dirty === '1') patch.despawn = E.dDespawn.checked;
  if (Object.values(E.dBiasInputs).some((i) => i.dataset.dirty === '1')) {
    const bias = {};
    for (const g of BIAS_GOALS) {
      const inp = E.dBiasInputs[g];
      if (inp.value === '') continue;
      const v = Number(inp.value);
      if (!isFinite(v) || v < 0.1 || v > 3) { setStatus(E.dStatus, 'err', '目标权重范围为 0.1 到 3。'); return; }
      if (v !== 1) bias[g] = v;
    }
    patch.goal_bias = bias;
  }
  if (!Object.keys(patch).length) { setStatus(E.dStatus, '', '没有任何改动。'); return; }
  if (await sendPolicy(patch, E.dStatus, '策略已应用。')) {
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
  setText(E.dMode, d.available ? (MODE_LABEL[d.mode] || d.mode) : '已禁用');
  E.dKill.hidden = !p.active;
  E.dOn.hidden = p.active;
  E.dOn.disabled = !d.available;
  E.dDry.checked = !!p.dry_run;
  E.dDry.disabled = !d.available;
  setText(E.dNote, d.available
    ? (p.active
      ? (p.dry_run ? '演练：不会启动或停止任何机器人；日志显示本来会做什么。' : '实时：调度器会在上限内自行启动和停止机器人。')
      : '已关闭：调度器不做任何事。')
    : '配置中已关闭调度器（[population] enabled = false）。数值仍会显示，但无法开启。');

  // sides
  const st = mkTable('sides', [{ t: '阵营' }, { t: '玩家', n: true }, { t: '机器人', n: true, title: '在世界中或正在登录' },
    { t: '自有', n: true, title: '由调度器启动' }, { t: '目标', n: true }, { t: '' }]);
  for (const s of d.sides || []) {
    const tgt = Math.max(num(s.target), num(s.bots), 1);
    st.tBodies[0].append(row([{ el: sideSpan(s.alliance) }, { t: num(s.players), n: true }, { t: num(s.bots), n: true },
      { t: num(s.owned), n: true }, { t: num(s.target), n: true }, { el: bar(num(s.bots) / tgt) }]));
  }
  const pace = h('div', 'muted pop-note',
    '最近一分钟启动：' + num(d.spawned_last_min) + ' / 上限 ' + num(d.max_spawn_per_min)
    + ' · 操作间隔至少 ' + num(d.min_action_gap_secs) + ' 秒'
    + (p.active && d.next_action_secs ? ' · 下次操作还有 ' + num(d.next_action_secs) + ' 秒' : '')
    + ' · 调度器名下共 ' + num(d.owned) + ' 个机器人');
  E.dSides.replaceChildren(st, pace);

  // health
  const hl = d.health || {};
  const kv = h('dl', 'kv');
  const add = (k, v) => { kv.append(h('dt', null, k), h('dd', null, v)); };
  add('内存', fmtBytes(num(hl.rss_mb) * 1048576) + (p.backoff_rss_mb ? '（上限 ' + p.backoff_rss_mb + ' MB）' : ''));
  add('每秒收包', fmtCount(hl.packets_in_per_sec) + (p.backoff_packets_in ? '（上限 ' + fmtCount(p.backoff_packets_in) + '）' : ''));
  add('每秒发包', fmtCount(hl.packets_out_per_sec));
  add('失败会话', num(hl.login_failures) + ' 次 / 5 分钟' + (p.backoff_login_failures ? '（上限 ' + p.backoff_login_failures + '）' : ''));
  add('未解决服务器问题', num(hl.open_issues) + (p.backoff_open_issues ? '（上限 ' + p.backoff_open_issues + '）' : ''));
  add('登录中', num(hl.connecting));
  if (hl.holding) add('暂停原因', hl.holding);
  E.dHealth.replaceChildren(kv);
  renderPresence(d.presence);

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
  if (!log.length) E.dLog.replaceChildren(empty('暂无记录。'));
  else {
    const lt = mkTable('log', [{ t: '时间' }, { t: '动作' }, { t: '原因' }]);
    for (const r of log) {
      const what = (KIND_LABEL[r.kind] || r.kind) + (r.count ? ' ' + r.count : '') + (r.alliance && r.alliance !== 'unknown' ? ' ' + (SIDE_LABEL[r.alliance] || r.alliance) : '');
      const why = h('div');
      why.append(h('div', null, r.reason || ''));
      if (r.result) why.append(h('div', 'muted', r.result));
      const tr = row([{ t: fmtClock(r.at_ms) }, { t: what + (r.applied || r.kind === 'policy' || r.kind === 'result' || r.kind === 'wish' ? '' : '（未执行）') }, { el: why }]);
      if (!r.applied && (r.kind === 'spawn' || r.kind === 'despawn')) tr.className = 'dim';
      lt.tBodies[0].append(tr);
    }
    E.dLog.replaceChildren(lt);
  }
}

// ------------------------------------------------------------------ 在线日程（presence）
// 一个块：第一次渲染时建立在"进程健康度"下面，数据来自 d.presence。
const CHRONO_LABEL = { early_bird: '早起的鸟', daytime: '白天型', evening: '夜猫子', night_owl: '深夜党' };
function presenceCurve(values, localTime) {
  const v = (values || []).map(num);
  const top = Math.max(1, ...v);
  const hour = parseInt(String(localTime || '').slice(0, 2), 10);
  const box = h('div');
  box.style.cssText = 'display:flex;align-items:flex-end;gap:1px;height:44px;margin-top:4px';
  box.title = '一天中每个小时排班的 bot 数（未算上下限）';
  v.forEach((n, i) => {
    const b = h('div');
    b.style.cssText = 'flex:1 1 0;min-width:2px;border-radius:2px 2px 0 0;background:var(--'
      + (i === hour ? 'warn' : 'accent') + ');height:' + Math.max(1, Math.round((n / top) * 40)) + 'px';
    b.title = String(i).padStart(2, '0') + ':30 · ' + n + ' 个 bot';
    box.append(b);
  });
  const axis = h('div', 'muted pop-note', '00:00 … 23:59 · 现在 = ' + (localTime || '?'));
  const out = h('div');
  out.append(box, axis);
  return out;
}
function renderPresence(pv) {
  if (!E.pBlock) {
    E.pHead = section('在线日程', '（登录日程）');
    E.pBlock = h('div', 'pop-block');
    E.dHealth.after(E.pHead, E.pBlock);
  }
  E.pHead.hidden = E.pBlock.hidden = !pv;
  if (!pv) return;
  if (!pv.enabled) {
    E.pBlock.replaceChildren(empty('已关闭（[presence] enabled = false）：所有 bot 保持在线。'));
    return;
  }
  const off = num(pv.utc_offset_hours);
  const tiles = h('div', 'pop-tiles');
  tiles.append(
    tile('本地时间', pv.local_time || '?', 'UTC' + (off >= 0 ? '+' : '') + off),
    tile('在线', fmtCount(pv.online), fmtCount(pv.own_online) + ' 个按日程，池 ' + fmtCount(pv.pool)),
    tile('当前排班', fmtCount(pv.scheduled), '目标 ' + fmtCount(pv.target) + ' 个在线'),
    tile('最近一小时', num(pv.logins_last_hour) + ' 个上线', num(pv.logouts_last_hour) + ' 个下线'),
    tile('上下限', num(pv.min_online) + ' – ' + (num(pv.max_online) ? num(pv.max_online) : '∞'), 'bot 同时在线总数'),
    tile('等待中', num(pv.waiting) + ' 个忙碌', num(pv.leaving) + ' 个告别，' + num(pv.held) + ' 个暂缓'),
  );
  const next = mkTable('presence-next', [{ t: '时间' }, { t: 'Bot' }, { t: '变动' }]);
  for (const c of pv.next || []) {
    next.tBodies[0].append(row([{ t: fmtAge(c.in_secs) + '后' }, { t: (c.name || '#' + c.id) + ' (' + c.id + ')' },
      { t: c.login ? '上线' : '下线', cls: c.login ? 'bt' : '' }]));
  }
  const chrono = mkTable('presence-chrono', [{ t: '作息' }, { t: 'Bot 数', n: true }, { t: '在线', n: true },
    { t: '排班', n: true }, { t: '' }]);
  for (const r of pv.chronotypes || []) {
    chrono.tBodies[0].append(row([{ t: CHRONO_LABEL[r.chronotype] || label(r.chronotype) }, { t: num(r.bots), n: true },
      { t: num(r.online), n: true }, { t: num(r.scheduled), n: true }, { el: bar(num(r.online) / Math.max(1, num(r.bots))) }]));
  }
  E.pBlock.replaceChildren(
    tiles,
    presenceCurve(pv.curve, pv.local_time),
    (pv.next || []).length ? next : empty('一天内没有变动。'),
    chrono,
  );
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
  ctx.textAlign = 'right';
  ctx.textBaseline = 'top';
  ctx.fillStyle = v.colors.label;
  ctx.fillText('热力：红 = 玩家，蓝 = 机器人（512 米方格）', W - 10, 8);
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
