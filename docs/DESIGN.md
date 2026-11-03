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
methods. Operator documentation: the README section "Server issue reports".

## Travel hooks (navigation motion, portals, dungeons, sailing)

The four travel features plug into the bot through fixed hooks, so each one edits only its own
files. Until a feature fills its hook it does nothing and the bots behave as before.

| Hook | Where | Owner |
| --- | --- | --- |
| `Motion::on_path_ready(ticket, PathResult)`, fed by `BotCmd::PathReady`; `BotDetail::path` (hub slot `path`) | `bot/motion.rs`, `bot/cmd.rs`, `hub.rs`, pb-web `types.rs` | navigation motion |
| `TravelState::on_event` (`BotState::travel`): the portal events | `bot/travel/` | portals |
| `Dungeon` goal, `goals::dungeon::{base_weight, score}`, `PartyOrder` / `BotCmd::Party` | `bot/goals/dungeon.rs`, `bot/party.rs` | dungeons |
| `Sail` goal, `goals::sail::{base_weight, score}`, `VehicleState::on_event` (`BotState::vehicle`): the slave and ship events | `bot/goals/sail.rs`, `bot/vehicle/` | sailing |
| `Place` (`BotState::place`): main world or instance world, `loading` between `SCProcessingInstance` / `SCLoadInstance` and the bot's `CSInstanceLoaded`; a change of world drops the entity view and the combat target; inside an instance motion gets no terrain heights (`Place::ground`) | `bot/place.rs` | shared (done) |

`[behavior] portals`, `dungeons` and `sailing` (default on) are read from `ctx.cfg.behavior` by the
features. `GoalKind::Dungeon` and `GoalKind::Sail` are scored only by their own `score` functions
(0 while a feature is a stub, so no bot picks it); forcing a stub from the web UI makes it fail with
`NotImplemented`, which switches it off for that bot. A ship's `SCOneUnitMovement` is decoded twice
(`Decoded::Ship`): the ordinary position event and `WorldEvent::ShipMoved` (heading, speed, helm).

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
