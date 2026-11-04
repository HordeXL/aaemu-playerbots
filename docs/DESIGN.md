# aaemu-playerbots — design

Player bots for the AAEmu Rust server (ArcheAge 1.2, r208022). Every bot is a **headless network
client**: it logs in through the login server, creates/selects a character and plays over the
normal client protocol. The server needs no changes; to it a bot is an ordinary player.

The behaviour is a re-implementation of the logic of
[NulightJens/ArcheAge-PlayerBots](https://github.com/NulightJens/ArcheAge-PlayerBots) (a C#
server-side mod, GPL-3.0-or-later) for a client-side bot, plus housing, trade runs, player trade
and an operator web UI, which the original does not have.

## Goals

- Hundreds of bots in one process, small memory per bot (target < 256 KiB of bot-side state).
- Bots quest, grind, duel each other, trade with each other, run trade packs and own houses.
- A web UI shows every bot on the world map with its activity, and accepts commands.
- An optional cheap LLM (any OpenAI-compatible endpoint, e.g. DeepSeek) gives bots chat replies
  and occasional high-level goal choices. Everything works without it.

## Crates

| Crate | Path | Role |
| --- | --- | --- |
| `aa-proto` | `crates/proto` | Wire protocol: framing, `PacketStream`, position codec, opcode tables, login client, game connection, CS packet builders, SC packet parsers → `ServerEvent`. No game logic. |
| `aa-world` | `crates/world` | Static world knowledge, loaded once and shared read-only (`Arc<World>`): compact.sqlite3 subset (NPCs, factions, quests and their start requirements, skills with mana by level, doodad and item use skills, crafts with their requirements, specialty buyers and pack prices, housing with garden radii), server spawn files, spatial index, terrain heights (mmap of the baked grid), the navigation meshes (`nav`: locate, weighted A* + funnel, component tables; the main world's and the lazily opened instance worlds'), baked extras (housing areas, quest spheres, zones, map calibration). |
| `aa-bake` | `crates/bake` | One-off extraction from the 1.2 client `game_pak` into the data dir: height grid, world map PNG + calibration, housing area polygons, quest sign spheres, zone sector grid, navigation meshes (the client's `.bai` navmesh as one memory-mappable file per world). |
| `pb-web` | `crates/web` | Operator web UI (axum): JSON API + an embedded single-page map UI. Knows nothing about bots except the `BotHub` trait and snapshot types it defines. |
| `pb-llm` | `crates/llm` | Optional OpenAI-compatible chat client with rate limit, concurrency cap and daily token budget. |
| `playerbots` | `crates/playerbots` | The binary: config, roster, bot runtime (connection lifecycle, world view, movement, combat), goals (quest, grind, duel, trade, trade run, housing, rest), coordinator, web hub, CLI (`run`, `bake`, `grant-gm-sql`). |

Dependency direction: `playerbots → {aa-proto, aa-world, aa-bake, pb-web, pb-llm}`; `aa-bake →
aa-world` (only for shared baked file types); nothing else depends on anything internal.

## Runtime model

- One tokio multi-thread runtime (2–4 workers). **One task per bot** owns its TCP connection and
  all its mutable state; it `select!`s over (a) the next server packet, (b) its brain tick timer,
  (c) its command channel. No per-bot reader task, no locks on the hot path.
- `Arc<World>` is shared read-only by all bots.
- Each bot publishes a small `BotSnapshot` (≈100 bytes) into the hub about once per second; the
  web UI only ever reads snapshots.
- Bots that need a partner (duel, trade, party) ask the **coordinator** (one task) over a channel;
  it pairs bots and sends both a command. Web UI commands go through the same per-bot command
  channels.
- Ticks are adaptive with jitter (combat 250 ms, moving 250 ms, idle 1–3 s, far from any real
  player and idle: 5 s), which avoids the C# version's 10-phase thundering herd and its serial
  loop over all bots.
- Bounded everything: entity view (only what the server shows), event log ring (32 lines),
  channels (small capacities).

## Protocol facts (verified against the server source)

- Frames: `[u16 LE len][payload]`. C→S level 1: `DD 01 00 <count> <u16 op> body`; level 2 (proxy):
  `DD 02 <u16 op> body`. No encryption, no checksum. S→C level 4 `DD 04 <u16 n> <deflate>` batches.
- Keep-alive: proxy `PingPacket` at least every 30 s (bots: every 5 s).
- Login: Trion ticket (XML, SHA-256 hex password), `AutoAccount` creates unknown accounts.
- Movement is not validated by the server (no speed clamp or collision check; > 25 m/s is only
  audit-logged). Bots must still move plausibly: run speed ≈ 5.4 m/s, Z from the terrain grid.
- The server checks distances (loot 5 m, specialty sale 2.5 m, skill range) against the position of
  the *last* `CSMoveUnit`, and a walking bot sends one only every second when no player is near. So
  the motion controller sends a final stopped update on arrival and `Motion::pin` puts an exact
  update in front of every packet the server range-checks (loot, talk, quest steps, craft, sell,
  casts); loot windows are opened from at most 3.5 m.
- GM commands are chat lines starting with `/`; the effective access level is
  max(account, character); most useful commands need 100. Bots use them only when
  `gm_bootstrap = true` (see "GM bootstrap").

## Goals and behaviour

Each bot has a **persona**: an archetype (class + rotation, from the original's
`BotArchetypes.json`/`BotRotations`) and temperament weights (quester, duelist, trader, merchant,
homebody). A utility scorer picks the next goal when the current one finishes or fails; the
optional LLM director may choose among the same eligible goals instead.

| Goal | Summary |
| --- | --- |
| Quest | Pick an eligible quest (story first, then nearest side quest), walk to the giver, accept, do the objectives (talk, kill, gather from loot, use item, interact with doodad, reach sphere), turn in. Failures back off; repeated failures quarantine the quest (typed reasons, not strings). |
| Grind | Kill level-appropriate hostile NPCs near the bot (fallback when no quest is eligible). |
| Duel | Coordinator pairs two idle bots; they meet, challenge/accept, fight with their PvP rotation, end on the server's `SCDuelEnded`. |
| Trade | Coordinator pairs two bots; they meet, open a trade window, put up an item and/or money, lock, confirm. |
| Trade run | Walk to a specialty workbench, craft a trade pack (`CSExecuteCraft`), carry it to a specialty NPC that buys it, sell (`CSSellBackpackGoods`), collect the payment mail. |
| Housing | Get a design (and tax certificates), walk to a free spot inside a housing area, quote (`CSConstructHouseTax`) and place (`CSCreateHouse`); later "go home" when resting. |
| Rest | Stand still out of combat until HP/MP recover. |
| Social | Reply to whispers/local chat from real players (LLM if enabled, canned lines otherwise). |

### GM bootstrap

Trade runs and housing need materials, labor, tax certificates or gold that a fresh level-1
character does not have. With `gm_bootstrap = true` and access level 100 on the bot accounts, bots
give themselves exactly what the goal needs (`/item add self …`, `/labor …`, `/gold add self …`)
and may use `/move` for long travel. Without it those goals are skipped with a visible reason.
`playerbots grant-gm-sql` prints the SQL that grants access level 100 to the bot accounts
(`WHERE username IN (...)` with exactly the accounts of the roster; `--game-db` / `--login-db` name
the MySQL databases when the server does not use `aaemu_game` / `aaemu_login`). The bot accounts
are GM accounts then, so `gm_bootstrap` is refused with the public default or an empty password;
without an explicit `server.password` / `$PLAYERBOTS_PASSWORD` the first run generates a random
one into `roster.secrets.json` (`secrets.rs`).

The server runs some GM commands on the sender's target and falls back to the sender only when
there is none, and a bot can end up targeting a real player (a duel). So every GM line is written
through `Ctx::gm` (`gm.rs`): an allowlist that rewrites `/level`, `/labor` and `/move` into the
explicit own-name form, passes only the `self` forms of `/item add` and `/gold add`, refuses
everything else, and puts `CSChangeTarget(0)` in front in the same flush. `/build` (which needs the
house as target) has its own entry point that targets the house first. The target is also cleared
when a fight against a character ends. Plain chat goes through `cs::chat`, which turns a
command-like text into nothing; `cs::gm_command` is the only builder that keeps a leading `/`.
Bots also decline trade requests from real players unless `behavior.trade_with_players` is on (and
never when they can mint goods).

## Quest progression (`goals/quest/progress.rs`, `aa-world::quest_atlas`)

The quest picker looks for a giver within 300 m, then within 1500 m. A bot that has done everything
in that circle used to stay there (grinding and wandering filled in), so the Harani and Firran
hand-off from Villanelle to the Silent Forest, 2 km apart, never happened. Now a bot whose scan
finds nothing three times in a row (5 s apart) plans a **trip**, and the picker itself ranks quests
a little differently.

**The atlas.** `World::quest_atlas` is built once, by the first bot that asks (about 80 KiB, listed in the memory breakdown): per quest, at most three giver spawns (position,
zone, level, story flag), sorted by zone, plus each race's **story chain**: the open story category
with the most quests (Nuian 3, Elf 8, Harani 62, Firran 61), in chapter order. It holds only quests
whose objectives all have a plan (`objective::supported`). It is race-independent; whether a bot
may take a quest is judged per bot (`Eligibility`: level with the usual +1 slack, done, in the
journal, quarantined, prerequisites, the race/gender/quest gates of the start, a giver that is not
hostile).

**Choosing a destination** (`plan_target`, pure functions in `progress.rs`, in this order):

1. **Story.** The giver of the first quest of the bot's chain that it qualifies for and that lies
   beyond 1500 m. Chain order, then level; unsupported and quarantined quests are skipped.
2. **Zone.** The zone whose eligible quests, weighted by how well they fit the bot's level, add up
   to the most (at least 1.5), then the densest cluster of its givers (400 m), discounted by
   distance.
3. **Grind.** A spot with monsters of the bot's level near the zone whose quests it will be able to
   take within a few levels. Reaching it fails the goal with `NoTarget`, so the brain's grinding
   takes over; no further grinding trip is planned for ten minutes.

A candidate is dropped when it is blocked (a failed destination, below) or risky: monsters
10 or more levels above the bot at the destination, 14 or more along the route (bosses excluded
there, they stand off the road), or a destination on a part of the navigation mesh the bot cannot
reach. The picker ranks by story, chapter, then "worth the time", then distance: a quest more than
6 levels below the bot (a side quest, never a story quest) comes after every other one, and a
bot with only those near goes to a better place instead of farming them.

**Walking.** The goal has a fifth mode, `Travel`, that uses the existing `walk()` legs (the
navigation mesh in pieces of up to 1500 m, at most ten, the straight line where there is no mesh;
`Ctx::travel` and the portal journey for trips over 1200 m; `/move` only with
`long_travel_teleport_m`). Every five seconds it rescans; as soon as a non-trivial quest is within
1500 m the trip ends as `InRange` and the picker takes it.

**State and back-offs.** `Memory.progress: Option<Box<Progress>>` (one pointer per bot, allocated by
the first scan) holds the trip, the empty-scan count, failed destinations and the last few
trips. A trip has its own time limit (distance at 3 m/s plus 3 min, between 5 and 45 min) that the
run's end does not cut short, and survives a rest or a duel in between. A destination that timed
out, was blocked or killed the bot twice rests for 10 min, then 20, then 40, together with
everything within 400 m of it. A quest in the journal that is only resting after a failure and may
be tried again within 2 min keeps the bot where it is (no trip is planned: a short back-off must not
send a bot across the map). A search that finds no destination waits 2 min before the next. A scan
that comes back empty within 90 s of arriving marks the place as barren.

**Visible.** The goal detail reads "heading to Lilyut Hills for quest 1234 Name (2.1 km to go)" /
"... for quests suited to level 17" / "... to hunt monsters"; the log (`info`) has one line at the
start ("progress: heading to ...") and one at the end ("progress: trip to ... arrived after 612 s
(0 m of 2034 m to go)").

**Not covered.** Zone-to-zone connections that need a carriage, an airship or a ferry (the walk
fails and the destination rests); crossing a hostile zone is only avoided where the danger checks
see it. The real-data tests (`tests/quest_real_data.rs`, ignored, need `AA_COMPACT` and
`AA_SERVER_DATA`) run each race for 240 simulated minutes and print quests done, zones visited and
trips.

## Improvements over the original (from the audit)

- Immutable, shared compiled rotations (`Arc`) with a tiny per-bot state, instead of per-bot
  compiled object graphs with embedded mutable state.
- No per-tick allocation in the brain; no world scans (the bot's entity view is maintained
  incrementally from spawn/despawn/move packets; static lookups use a grid index).
- Scheduler with per-bot jittered deadlines instead of one serial loop with 10 phases.
- Typed failure reasons instead of string matching for quest quarantine.
- Buff checks keyed by buff ids actually observed (fixes `buffMissing` comparing a buff id with
  a skill id), zero-guarded HP percentages.
- Optional LLM, rate-limited and budgeted, never on the tick path.

## Server issue reports

Bots are the first players of a new server, so they also find its bugs. `crates/playerbots/src/issues/`
is the process-wide plumbing; the detectors (what counts as a problem) live in the bot loop and the
goals and file an `IssueReport` through `Ctx::report_issue` / `Bot::file_issue` → `Hub::report_issue`.

- `model.rs` the report (kind, stable key, title, expected/actual, suspect, severity, area hint, facts,
  and the bot context, packet trail and logs the bot side attaches), the fingerprint `pb:<8 hex>` (FNV-1a
  of kind and key; it is the marker in the team's queue file), `TrailRing`.
- `book.rs` `IssueBook`: dedup by fingerprint, counts, distinct bots, three samples, state
  (open / queued / ignored / resolved), the reportable policy, the queue rate limits, eviction at 500
  issues, JSON persistence. Plain data and no clock: tested without a runtime.
- `service.rs` `Issues`: the book behind a mutex (never held across an await) plus three background
  tasks: the debounced saver, the sink (evidence folders and queue entries, recurrence checks) and the
  server log tail. `report` never waits for I/O.
- `queue.rs` / `evidence.rs` the append-only entry for the team's issue file and the evidence folders;
  `logtail.rs` the server log follower and its bounded ring; `redact.rs` removes addresses and
  credential values from every text; `timefmt.rs` local times; `view.rs` the web types.

The detectors are `crates/playerbots/src/bot/watch/`, one `Box<Watch>` per bot in `BotState` (a single
allocation; `size_of::<Bot>()` grew by 24 bytes for it):

- `tracker.rs` a fixed table of eight requests the server must answer (quest accept/complete, loot
  open, skill cast, GM `/move`) with a 6 s deadline. Requests are registered from the outgoing packet
  in one place (`Tracker::on_sent`), cleared by the matching server event, and forgotten when the bot
  died, was teleported or moved away, or when nothing at all arrived since (then it is a stall).
- `classify.rs` the judgement as pure functions: whose fault a skill refusal is, when a quiet line is a
  sustained stall, whether a counter that did not move is a missing credit, whether a monster that
  takes no damage is immune, what a finished trade must have changed, what a bad payment mail is.
- `finding.rs` `Finding` (a `Copy` note a detector leaves) and its report: kind, id-only key,
  title, expected/actual, suspect and severity. The `Names` trait keeps it testable without a `World`.
- `mod.rs` `Watch` (trail, tracker, connection and death watches, findings, per-session episode
  dedupe) and the connection watch.

The flow is note, then drain: a detector pushes a finding, the bot loop drains them after every packet
and tick and files them with the packet trail attached (`TrailRing`: 8 bytes per packet, 48 packets,
allocated on the first packet, names resolved from the opcode tables only when a report is made).
Nothing runs per tick except a few comparisons; an episode is reported once.

The web crate only knows `IssueSummary` / `IssueDetail` / `IssueAction` and three defaulted `BotHub`
methods. Operator documentation: [`ISSUES.md`](ISSUES.md).

## Travel hooks (navigation motion, portals, dungeons, sailing)

The four travel features plug into the bot through fixed hooks, so each one edits only its own
files. Until a feature fills its hook it does nothing and the bots behave as before.

| Hook | Where | Owner |
| --- | --- | --- |
| `Motion::on_path_ready(ticket, PathResult)`, fed by `BotCmd::PathReady`; `BotDetail::path` (hub slot `path`) | `bot/motion.rs`, `bot/nav/`, `bot/cmd.rs`, `hub.rs`, pb-web `types.rs` | navigation motion (done) |
| `TravelState::on_event` (`BotState::travel`): the portal events | `bot/travel/` | portals |
| `Dungeon` goal, `goals::dungeon::{base_weight, score}`, `PartyOrder` / `BotCmd::Party` | `bot/goals/dungeon.rs`, `bot/party.rs` | dungeons |
| `Sail` goal, `goals::sail::{base_weight, score}`, `VehicleState::on_event` (`BotState::vehicle`): the slave and ship events | `bot/goals/sail.rs`, `bot/vehicle/` | sailing |
| `Place` (`BotState::place`): main world or instance world, `loading` between `SCProcessingInstance` / `SCLoadInstance` and the bot's `CSInstanceLoaded`; a change of world drops the entity view and the combat target; inside an instance motion gets no terrain heights (`Place::ground`) | `bot/place.rs` | shared (done) |

`[behavior] portals`, `dungeons` and `sailing` (default on) are read from `ctx.cfg.behavior` by the
features. `GoalKind::Dungeon` and `GoalKind::Sail` are scored only by their own `score` functions
(0 while a feature is a stub, so no bot picks it); forcing a stub from the web UI makes it fail with
`NotImplemented`, which switches it off for that bot. A ship's `SCOneUnitMovement` is decoded twice
(`Decoded::Ship`): the ordinary position event and `WorldEvent::ShipMoved` (heading, speed, helm).

## Market hooks (quests, auction, life-like movement, trade-chat sales)

Four more features plug in the same way. Shared and done: the goals, the configuration, the item
facts, the provenance ledger and the chat forwarding.

| Hook | Where | Owner |
| --- | --- | --- |
| `Auction` goal, `goals::auction::{base_weight, score}`, `AuctionMemory` (`BotState::market`, `Inner::auction`) | `bot/goals/auction.rs`, `bot/market/auction.rs` | auction house |
| `Market` goal, `goals::market::{base_weight, score}`, `AdState` (`Inner::ads`) fed by `AdState::on_chat` | `bot/goals/market.rs`, `bot/market/ads.rs`, `bot/goals/trade*` | trade-chat sales |
| `ItemFacts` / `World::item_facts(template)`: price, refund, stack, bind kind, level, fixed grade, auction categories and the quest-item flag of every item, 20 bytes each (about 0.5 MB for the whole table), `ItemFacts::tradable()` (sellable, unbound, not a quest item) | `aa-world` `items.rs` | shared (done) |
| `Ledger` (`MarketState::clean_count`, `sellable_surplus`): the quantity of each item the bot may sell, fed by `BotState::apply` from the item task reasons | `bot/market/ledger.rs`, `bot/state.rs` | shared (done) |
| `QuestNeeds::of(world, &state.quests)`: what the active quests still ask of each item (gather and use objectives), the part of the bag the sale paths keep back | `bot/market/needs.rs` | shared (done) |
| `MarketState::on_chat(kind, sender, text, now_ms)`: shouts, trade chat, group-finding chat and whispers, forwarded by the social duty before it files the inbox (`market::forward_chat`; which kinds: `ads::hears`) | `bot/market/`, `bot/goals/social.rs` | shared (done), kinds are the market feature's |
| `[behavior] lifelike`, `jump_rate`, `emotes` | `config.rs` | life-like movement (done, see below) |
| `[market]` | `config.rs`, `Config::may_sell_to_players` | shared (done) |

**What may change hands.** Every path that sells, asks for, buys or gives an item away goes through
`ItemFacts::tradable()`, and the paths that part with the bot's own goods also keep back
`QuestNeeds`: the sellable surplus (`Ledger::sellable_surplus`, behind the chat ads, the chat deals
and the auction listings), the trade-chat name index (`ads::names::global`), the WTB goods
(`goals::market::duty::wanted_goods`), the stand-in offers of random trades
(`goals::trade::choose_offer`, rule `keep_of`: unknown items are not offered) and the auction
buys (`auction::Env::tradable`). A quest item is detected from the compact database when the
item table loads (the quest item category, quest-only loot, and the items the quest tables hand
out except in the reward step); quest gather and use targets are ordinary materials and only
count in `QuestNeeds`. A chat deal re-checks `QuestNeeds` when the meeting starts, since a quest
taken since may need the goods.

`MarketState` is one pointer per bot (`Option<Box<Inner>>`, allocated by the first gain the ledger
counts), because the per-bot budget is tight (`size_of::<Bot>() < 3200`). The brain scores
`GoalKind::Auction` and `GoalKind::Market` only through their own `score` functions (0 while they
are stubs, so no bot picks them); forcing one from the web UI makes it fail with `NotImplemented`,
which switches it off for that bot. The brain's failure cooldowns are whole seconds in a `u32`
for the same reason.

## Navigation motion (`bot/nav/`)

Bots walk routes on the navigation mesh (`aa_world::nav`) instead of straight lines;
`[behavior] navmesh = false` brings the straight lines back.

- **Where it plugs in.** `Motion` keeps its straight-line walker, the stuck detector, the stopped update
  on arrival and `Motion::pin`. A bot has a `NavLink` (`Motion::attach_nav`, built by
  `nav::link_for_bot`) and `Motion::go_to` starts a `NavState` trip; each tick the trip either moves
  the bot along its route or says "walk the straight line" (`NavStep::Legacy`). The goals' API
  (`go_to`, `travel_to`, `stop`, `face`, `status`, `remaining`, `pin`) did not change; `sync_place`
  tells the controller which world's mesh to use (main, instance, or none).
- **Planning** (`trip.rs`, `planner.rs`). A trip whose goal is within 150 m is searched on the bot's
  own task (cap 12k expansions). A longer one goes to the planner pool: one or two `std` threads and
  a bounded queue (512), owned by the hub (`Hub::nav`, started with the first bot). The answer
  comes back as `BotCmd::PathReady { ticket }` on the bot's own command channel. A full queue, an
  answer later than 1.5 s, `Unreachable`, `NoMesh` (after a second try that snaps from 24 m) all
  mean the straight line. While it waits the bot stands and faces the goal. A partial answer, or a
  goal beyond 2 km, is walked in pieces: the search for the next piece starts the moment a route is
  installed, from its end (at most 10 pieces). Tickets make stale answers harmless.
- **Walking** (`follow.rs`). `Route` is a parametric curve over the path's corners: outdoor corners
  are rounded by a quadratic fillet (start up to 2 m before, never more than 0.3 m inside the
  corner, because the funnel's agent-radius clearance must survive), indoor and waypoint corners are
  exact, long stretches get a small per-bot lateral offset that fades at both ends and is only used
  where the mesh has room beside the line. Heights come from the terrain outdoors and from the
  path indoors and in instances. Run outdoors, walk indoor segments (unless longer than 25 m), slow
  to walk speed over the last 5 m before the stop point. The stop point is the circle of the stop
  radius around the goal, like the straight line.
- **Stuck.** The old detector (under 0.3 m in 3 s) first re-plans twice with the nodes on and
  ahead of the bot made expensive, then falls back to the straight line's side steps and finally
  `Failed(Stuck)`.
- **Time limits.** A planned route may take `elapsed + length / 3 m/s + 30 s` (never less than the
  90 s of the straight line), so legs of 1.5 km work; goals that cut walks into legs
  (`trade_run`) ask `Motion::leg_m()` (1500 m with the mesh, 280 m without).
- **Numbers.** `NavMetrics` (atomics in `Hub::nav`): searches inline / pooled, outcomes, latency
  histograms (p50 / p99), replans, stuck events, straight-line fallbacks by cause, queue depth,
  trips and how they ended, detour ratio. They are in `HubStats::nav` (`/api/stats`) and logged
  once a minute.
- **Web.** `Bot::publish` writes the rest of the route (at most 32 points, Ramer-Douglas-Peucker
  decimated) into the hub slot; the map draws it for the selected bot.

## Life-like movement (`bot/manner.rs`)

A bot that only walks and stands still looks like a bot. With `[behavior] lifelike` (default on) the
movement controller adds what a person does, **for the eyes of a real player only**: nothing happens
unless a character that is not one of our bots is within the watch radius (the same signal that
raises the movement update rate), and nothing while the bot fights, loots, casts, trades, sails, is in
an instance, dead or loading (`Bot::life_busy`). `jump_rate` scales the jumps (0 = none), `emotes`
switches the emotes.

- **Where it plugs in.** `Life` (traits, jump state, schedule, mannerisms, emote state; one
  `Box` per bot, `Option<Box<Life>>` in `Motion`, so the bot gained 8 bytes) is owned by `Motion`.
  `Motion`'s public API did not change; its tick calls the parts and `Bot::on_tick` calls
  `manner::tick` for the emotes and to tell the controller the moment (`Gate`: audience, busy, town).
  The parts are plain values without clock or socket (`Traits`, `Arc`/`Jump`, `JumpSched`, `Fidget`,
  `Dawdle`, `Emotes`, `Budget`), so the rates and limits are unit-tested over simulated hours.
- **Wire fixes** (`aa_proto::cs::move_unit`): velocity shorts are `m/s * 32767/60` (the client's
  scale; they were `m/s * 1000`, which a remote client reads as 1.8 times the speed), a
  walk has actor flag `0x01` and half the stick (`delta` 63), a stop has actor `0x04` and no
  velocity, a swimming bot sends stance 2.
- **Jumps.** The client's physics: 1.3 m, gravity 25 m/s^2, so 8.06 m/s up, apex after 0.32 s, 0.65 s
  in the air (a hop over a step: 0.55 m). Packets go at take-off (header `0x04`, `0x06` with a stick
  held, actor `0x10`, `vel.z` up), at the apex and on the way down (header `0x00`/`0x02`, actor
  `0x00`, height in `pos.z`), at touchdown (header `0x04`, actor `0x14`, no vertical speed) and 90 ms
  later as an ordinary ground update. The bot's real position stays on the ground; the height is only
  in the packets. A running bot keeps its horizontal speed. A jump wakes the bot loop for its next
  packet (`Motion::wake_in`, at least 15 ms apart); the regular updates wait while it lasts.
  Triggers: a straight stretch of 6 m or more while running (one per 30 to 60 s times the bot's
  multiplier, 10 s cooldown, one in ten is two in a row), a step of 0.3 to 1.2 m ahead (a hop, 15 s
  cooldown), after the stuck detector's side-step, in place after 90 to 180 s of standing, and to
  cheer a level or a finished quest. A bot's playfulness comes from its id: 12 % never jump, the rest
  jump 0.35 to 1.65 times as often as the rate. A pin, a stop, a teleport or a new height ends a jump
  (a bot never jumps with a range-checked packet queued, in combat, swimming, sitting, or while a
  teleport waits).
- **Mannerisms.** After a trip a standing bot turns in place one to three times (30 to 120 degrees,
  2 to 6 s apart) and sometimes shuffles 0.5 to 1.5 m at walk pace; at some corners of a long route it
  stops for 0.3 to 1 s; in a town (the peace aura or a crowd of eight) a quarter of the short trips
  are walked; every bot runs within 3 % of the configured speed; in front of a player the tick period
  varies by 20 % either way (the mean, so the packet rate, does not change).
- **Emotes** (`CSExpressEmotion`, `SCEmotionExpressed`). Greeting (a player within 6 m, 60 % of the
  time, once per player per 10 min), an answer to an emote of a real player (90 % when played at the
  bot, 30 % nearby, at most one per player per 30 s), ambient emotes in towns (at most one per 45 s
  per bot), a celebration with a jump for a level or a quest, and the sit emote (id 48) 8 to 25 s into
  a rest; the stand-up is a stopped update. Each bot waits 8 s between emotes, and a process-wide
  allowance (5 ambient and 15 social a minute) keeps the server log quiet, because the server logs
  every emote at WARN.
- **Ground truth.** What the real 1.2 client sends was captured with a bot that logs the relayed
  packets (`RUST_LOG=pb_move=trace`): a standing client sends header `0x00`, actor `0x04` about every
  3 s; walking, about seven updates a second with header `0x02`, actor `0x04` and a vertical speed
  on slopes; a stop is two updates 47 ms apart (`0x04`, the second without velocity; bots send one);
  a fall is header `0x00`, actor `0x00` and ends in `0x04`, actor `0x14`. The **take-off flags of a
  jump and the emote packets are not confirmed by a capture** (the client driver could not press the
  jump key or run an emote); they follow the server's `MoveTypeFlags` (`Moving | Stopping` is a
  jump), the touchdown and the client code. `RUST_LOG=pb_life=debug` logs every jump and emote a bot
  makes.
- **Live check** (20 bots for 16 minutes near the Nuian start with real characters in range, against
  the same bots with `lifelike = false`). Jumps: 258 started, 48 per bot-hour (31 on the run, 17
  hops), 18 of the 20 bots jumped. Emotes: 36, about 2.3 a minute, one server WARN line each (19
  celebrations, 11 ambient, 3 greetings, 3 sits). Outgoing packets per bot stayed within the
  run-to-run noise (1.6 to 1.9 a second with it on, 1.7 off, in the same place); the server log had no
  speed-audit or landing lines. The relayed packets (`pb_move`) showed every jump as the sequence
  above, the take-off and the touchdown always in pairs.

## Portal travel (`bot/travel/`)

Bots record Teleport Book entries, open gates to them and use gates; `[behavior] portals` switches it
all off.

- `book.rs` the bot's picture of the book: private entries (id, zone key, wire offsets, world
  position once known), visited recall districts (their positions come from `recalls.json`) and the
  bound recall point. The wire sends an entry as an offset from the origin cell of its zone
  (`ZoneOrigins` in `aa-world`, one cell per zone, learned from a recall district or from the spot the
  bot stood on when it recorded, shared by all bots and kept in `portal_zone_origins.txt` next to the
  roster).
- `plan.rs` the route as a pure function: walk, gate to the entry nearest the destination (within
  400 m) or recall, by estimated seconds; trips under 1.2 km are walked.
- `journey.rs` `Journey`, the state machine any goal or the party code drives: `Journey::new(dest)`
  opens a gate (top-up of the book, stones and ink with `Ctx::gm`, cooldown wait, cast, wait for the
  bot's own gate NPC 3891, get within 3.5 m, `CSUsePortal`, detect the move), or recalls;
  `Journey::follow_gate(owner)` goes through the gate that character opened. `step(&Snap) -> Acts +
  JourneyStatus` is pure and tested; every wait has a deadline and a typed `JourneyFail`
  (`NoRoute`, `NoBook`, `NoReagents`, `OpenRefused(code)`, `NoGate`, `GateGone`, `UseRefused(error)`,
  `NoTeleport`, ...). A failed journey never retries: the caller walks and the destination is walked
  for two minutes.
- `drive.rs` the glue: `Journey::tick(ctx)`, `far_trip` (the hook of `Ctx::travel`: a far trip goes to
  an implicit journey when the planner finds a route, goals need no change; `has_route` lets the trade
  run ask for a far trip in one piece instead of walking legs), `duty` (called by the brain every tick:
  steps the implicit journey, records entries, visits recall districts with `CSNotifySubZone` like the
  client, lets an idle bot hop through another bot's gate now and then, and keeps the goal waiting while
  a cast or a gate is under way) and the detectors that file issues (record cast with no entry, gate
  cast accepted and stones taken but no gate, `SCUnitPortalUsed` without a teleport, `CSUsePortal`
  refused or unanswered, refused skill starts).
- `places.rs` where a spot is worth an entry (a recall point within 120 m, two quest givers within
  60 m, a pack buyer within 40 m, a dungeon entrance within 40 m, the bot's house), the recording rules
  (`decide`: one record per 10 minutes, calm, no entry within 600 m, ink, book of at most 20 entries,
  the most redundant entry goes first) and the entry names.

Server rules the live run showed (all of them are data, not bugs): a bot with a trade pack gets
`SCErrorMsg` 610 on `CSUsePortal` and result 50 on the open skill (a buff of the pack), so a laden bot
plans no gate and opens or follows none (`JourneyFail::Laden`); the record skill 11215 is not on the
`skill_reqs` whitelist of the peace-zone buffs (2149 around a Nui statue, 2221, 4828), so it is refused
with result 50 there. `Portals::blocking_buffs(skill)` reads such buffs (and stun, landing, ...) from
`skill_reqs` for the record, open and recall skills; the bot waits for one to go (`JourneyFail::Blocked`
after 15 s) instead of casting into a refusal; with `bots.long_travel_teleport_m` set a GM
`/move` is faster than any gate, so far trips are left to it.

`TravelState` is one pointer (the data is a lazily created `Box<Inner>`); `size_of::<Bot>()` grew by 8
bytes. The server events it needs are routed by `state::route` (the portal events and the answers to
skills 11215, 11216, 10594 and 16841); `apply_unit_state` tells it about gate NPCs (with their owner).

## Sailing (goal `sail`)

A bot gets a boat summon scroll, summons a boat, sails it along a planned route and comes ashore.
Only the sea is planned: lakes and rivers are out of scope (the sea mask knows nothing of them).

```text
Gear -> Plan -> Shore -> Summon -> Board -> Sail -> Land            Abort (every failure, then Failed)
```

- **Gear**: a scroll in the bag is used; otherwise, with `gm_bootstrap`, `/item add self 17906 1`
  (the cheapest boat scroll of the data with a driver seat and a top speed of 4 m/s or more; the
  rowboat). A boat left over from an earlier trip is despawned first. Without GM and without a
  scroll the goal fails (`NoTarget`) and the brain's 60 s failure cooldown applies.
- **Plan** (`goals/sail/trip.rs`): the nearest beach with open water in front of it is the launch
  (`find_shore`: land 3 m behind the stand point, water 12 to 28 m off the beach); the landing is a
  beach near a random open-water point 1 to 3 km away whose sea route is at most 2.2 times the
  straight line; otherwise a sightseeing loop out to a point at sea and back to the launch. Aggressive
  monsters that live on the water (`danger_zones`: the spawns of NPC templates that attack on sight
  and that the bot's faction is hostile to, e.g. 8563 / 13012 near (15344, 16346); at most 400)
  close a 100 m disc each to the planner (140 m shuts the whole of Castaway Strait to a trip).
- **Sea map** (`aa-world::water`, `World::water()`): 16 m cells from the 4 m height grid, built on
  first use by up to four threads (about 1.4 s in a debug build, a tenth in release) and shared by
  all bots: a cell is sea when every height sample it touches is a real terrain sample under 98 m.
  35 x 2431 cells of bits, **664 KiB**. The planner is an A* over open cells (a cell is open when
  its square neighbourhood of `clear` cells is all sea) with hashed scratch, a node bound,
  a shore penalty and line-of-sight smoothing; the route keeps two cells (32 m) off the beach, one
  as a fallback.
- **Shore / Summon**: the bot walks to the stand point, faces the water, waits 0.8 s and casts the
  scroll's skill on itself (`start_skill_item`). Refusals of the place (`SkillResult` 0x23, error
  messages 100 / 583) pick another beach, at most two more times; an unexpected refusal is a
  `SkillRefused` finding; a boat that does not appear within the cast time plus 10 s is an issue.
- **Board**: the bot swims to the hull (the server measures the bind reach to the hull's mass box,
  under 4 m of surface gap) and sends `CSBindSlave` from 2.8 m, closer after a "too far" answer.
- **Sail** (`bot/vehicle/{helm,follow}.rs`): pure-pursuit on the waypoints (lookahead 70 m, waypoint
  reach 40 m, arrival 18 m) and a hull-aware rate-tracking heading controller. The server turns
  slowly (a rowboat 4.35 degrees per second, angular acceleration 0.125 rad/s^2 at full helm; both
  from `ship_models.turn_accel` and the kind's turn cap, `SlaveTemplate::turn`), so the controller
  asks for the yaw rate that stops on the target heading (`sqrt(2 a err)`), converts the rate error
  to a steering value and slows down for sharp turns. A steering probe at the start (80 steering
  with 100 throttle for 2.5 s) measures which way the wheel turns (the documented direction is a
  positive steering that lowers the yaw), and a divergence watch flips the sign once more if the
  heading runs away. A `ShipRequest` is sent when the throttle changes, the steering moves by 6 or
  more, or as a 1 s heartbeat (the server keeps a request until the next one).
- **Stuck**: less than 3 m of progress in 8 s with the engine on, or the server's `stuck` flag, backs
  the boat off astern for 4 s and plans the route again from where it is; the fourth episode aborts.
- **Land**: brake (astern while above 2.5 m/s), set the throttle to 0, `CSDiscardSlave`, then
  `CSDespawnSlave`, then walk (swim) to the landing stand point. Each request is repeated once
  after a timeout; a missing answer is an issue and the trip goes on.
- **Never leave a boat behind**: every way the goal ends (done, failed, interrupted by a rest, a
  forced goal, death) passes through `Sail::stop`, which stops the helm, leaves the seat and
  despawns the boat. A cast that is still in flight when the goal is interrupted can still put a
  boat on the water; the next sail goal despawns it first and the server removes it at logout.
- **Scoring** (`goals::sail::score`): 0 unless `[behavior] sailing`, outside instances, hit points
  60 percent or more, online 3 minutes or more, 30 minutes since the last trip of this bot, a scroll
  obtainable (bag or GM) and a coast within 450 m (terrain probes under 98 m); the weight is
  `0.2 * (0.3 + 0.7 * (1 - homebody) + 0.7 * merchant)`, so explorer- and merchant-like bots go
  more often than homebodies. `base_weight` stays 0 (the goal owns its weight).
- **Fast ticks**: `VehicleState::fast_tick` keeps the 250 ms bot tick while the goal has a boat or a
  seat (the idle tick is 1.5 s). On the boat the goal owns the combat decisions
  (`owns_combat`): a fight does not pause the helm; the combat controller only defends.
- **Detectors** (`ctx.report_issue`, one per key and bot): `sail:no_slave:<skill>` (cast accepted,
  no boat), `sail:bind_no_attach` (`SCSlaveBound` names the bot, no `SCUnitAttached`),
  `sail:bind_no_response`, `sail:no_ship_movement`, `sail:no_thrust` (engine on 5 s, deep water,
  not aground, speed 0), `sail:dead_wheel` (helm hard over at speed for 10 s, no turn),
  `sail:despawn_ignored`, `sail:discard_ignored`, `sail:error_code:<id>` (an unexpected
  `SCErrorMsg` after a boat action).
- **Memory**: `VehicleState` is one pointer (the `Sailor` is allocated by the first sail goal or
  summon and kept), `Sail` is one boxed `Machine` (a few hundred bytes while a trip runs), the sea map
  is 664 KiB once. `size_of::<Bot>()` went from 3152 to 3160 bytes (limit 3200), `Goal` is 152
  bytes (limit 160).
- **Live behaviour** (QA server, rowboat scroll 17906, slave 15): the boat takes 10 to 20 s from the
  cast to the bind, the helm probe finds the documented steering sign, the boat holds its heading
  within a degree on the straights and its top speed is 3.2 m/s (`ship_models.velocity` 6.4 divided
  by 2: the `MoveSpeedMul` of 2.0 the data estimate assumes is not in effect, so `sail:slow_boat`
  files a low-severity report). A crossing of 1.6 to 3.4 km takes 9 to 18 minutes, landing, despawn
  and the swim ashore about 20 seconds. The server keeps a despawned (or logged out with) boat in its
  `slaves` table and puts it back on the water at login, at its old position: the Gear phase
  despawns such a boat first. A scroll is not consumed by the cast.
- **Limits**: sea only; the bot that is made to leave the goal at sea (a forced goal, a rest) is left
  swimming where it is; passengers (`take_passenger_seat` on seat 2 of the rowboat) are not done.
- **Tests**: the water mask, the planner (detour, no route, discs), the shore finder, the heading
  controller (also with an inverted wheel and other hulls), the route follower, and the whole
  machine against a scripted server (`goals/sail/tests.rs`: a rowboat with the server's turn
  physics, GM scroll, refused places, no boat, no seat, ignored bind / discard / despawn, a dead
  engine, a boat aground, an interruption at every point of the trip, a late boat of an interrupted
  cast). Real-data check: `AA_BAKED=... cargo test -p aa-world --test water_real -- --ignored`.

## Data directory

`playerbots bake` writes (see `docs/DATA-FORMATS.md`):

```
data/
  heights_main_world.bin   # u16 grid, header + samples, mmap'd at runtime
  map_main_world.png       # painted world map (or relief fallback)
  map_main_world.json      # calibration: world rect covered by the image
  housing_areas.json       # polygons per housing area id
  quest_spheres.json       # quest sign spheres (quest id, component id, pos, radius)
  zones_main_world.bin     # zone key per 64 m sector (optional)
  nav_main_world.bin       # navigation mesh for path finding (mmap'd), one more per instance world
  nav_<instance>.bin
```

Runtime also reads the server's `compact.sqlite3` (read-only) and `app/Data/Worlds/*/` spawn
files, paths given in `playerbots.toml`.

## Travel data (`aa-world::travel`)

Static data for the portal, instance-dungeon and boat abilities, loaded once with the world
(about 180 KiB of heap) and read through `World::travel()` or the `World` shortcuts
(`recall_points`, `nearest_recall`, `portal_reagents`, `dungeons`, `dungeon_by_entrance_doodad`,
`instance_world`, `instance_world_by_zone_key`, `boat_scrolls`, `slave_template`,
`boat_for_scroll`).

- **Portals** (`portal.rs`): `Data/Portal/{recalls,worldgates,respawns}.json`, the open-portal
  reagent tables (1 stone on the same continent, 3 across), the Teleport Book / record / recall /
  Memory Tome skills with their timings, the gate NPCs 3891 / 6949 and the Memory Tome doodad
  spawns of the main world with the recall point each binds per faction.
- **Dungeons** (`dungeon.rs`, `instance.rs`, `funcs.rs`): the `DoodadFuncEnterInstance` entrance
  doodads that spawn in the main world, the `indun_zones` rules (level, party, item, `closed`),
  the entry limit (3 per zone group per 4 h) and every `Worlds/<name>/` instance world: zone keys,
  spawn points, NPC / usable doodad spawns and the exit doodads. The zone key an `SCLoadInstance`
  carries is matched to a world by `zones.name` (the client's `world.xml` is not server data).
- **Boats** (`boat.rs`): items whose use skill has the `SpawnSlave` effect and summon a boat-kind
  slave, with seats (`model_bindings`), `ship_models.velocity` and the slave's `MoveSpeedMul`
  (top speed = velocity x MoveSpeedMul / 2) and an estimated hit point pool.

Fixed ids (Teleport Book 4045, skills 11215 / 11216 / 10594 / 14602 / 16841 / 15802, ...) are
checked against `compact.sqlite3` at load and only warn when missing. Real-data checks:
`AA_COMPACT=... AA_SERVER_DATA=... cargo test -p aa-world --test travel_real_data -- --ignored`.
