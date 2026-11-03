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
(0 while they are stubs, so no bot picks them); forcing one from the web UI makes it fail with
`NotImplemented`, which switches it off for that bot. A ship's `SCOneUnitMovement` is decoded twice
(`Decoded::Ship`): the ordinary position event and `WorldEvent::ShipMoved` (heading, speed, helm).

### Portal travel (`bot/travel/`)

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
with result 50 there and the bot waits for the buff to go; with `bots.long_travel_teleport_m` set a GM
`/move` is faster than any gate, so far trips are left to it.

`TravelState` is one pointer (the data is a lazily created `Box<Inner>`); `size_of::<Bot>()` grew by 8
bytes. The server events it needs are routed by `state::route` (the portal events and the answers to
skills 11215, 11216, 10594 and 16841); `apply_unit_state` tells it about gate NPCs (with their owner).

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
