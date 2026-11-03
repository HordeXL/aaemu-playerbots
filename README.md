# aaemu-playerbots

Headless player bots for the [AAEmu Rust server](https://github.com/AAEmu/AAEmu-1.2-Rust)
(ArcheAge 1.2, r208022). Every bot is a network client: it logs in through the login server,
creates or selects a character and plays over the normal client protocol, so the server needs no
changes. Hundreds of bots run in one process (one tokio task per bot) and show up on a live map in
a small web UI.

## What the bots do

Each bot has an archetype (class build and skill rotation) and a temperament (how much it likes
quests, duels, trading, trade runs, staying home). A utility scorer weighs the goals below by the
configured weights, the temperament and the situation and draws the next one; goals that fail rest
for a minute. The bot defends itself whatever it is doing.

| Goal | What it does |
| --- | --- |
| **Quest** | Picks a quest (journal first, then story, then the nearest side quest), walks to the giver, accepts, does the objectives (talk, kill, gather, use an item or doodad, reach a sphere) and turns in. Failures back off and repeated ones quarantine the quest. |
| **Grind** | Kills level-appropriate monsters near the bot and loots them. |
| **Duel** | A coordinator pairs two idle bots; they meet at a flag, challenge, accept, fight with their PvP rotation and rest afterwards. |
| **Player trade** | The coordinator pairs two bots; they meet, open the trade window, put up goods and/or money, lock and confirm. A trade request from a real player is accepted when the bot is idle. |
| **Housing** | Picks a design and a free spot in a housing area, quotes the tax, places the house, builds it and later "goes home" to rest there. |
| **Trade run** | Crafts a trade pack at a specialty workbench, carries it to a specialty NPC that buys it, sells it and collects the payment mail. |
| **Rest / Wander** | Stand still until hit points and mana recover; stroll around. |
| **Social chat** | Answers whispers and nearby players (canned lines, or an LLM in character) and chats a little with other bots (see below). A background duty, not a goal. |
| **LLM director** (optional) | Now and then asks the LLM which of the currently eligible goals to do next (see below). |

Bots survive death (they request the resurrection), disconnects (reconnect with backoff), name
rejections (a new name is picked) and restarts (the roster remembers accounts and characters).
Names, races, classes and temperaments are stored in `roster.json`: keep it.

Housing and trade runs need materials, labor, tax certificates and gold that a fresh character does
not have; see "GM bootstrap". Without it those goals only work when the bag already holds what
they need, and the LLM director never offers them.

## Quick start

```sh
cargo build --release -p playerbots
playerbots example-config > playerbots.toml     # edit the paths in [data] and the login address
playerbots bake --pak <client>/game_pak --out <dir> --compact <compact.sqlite3>   # once, optional
playerbots run --config playerbots.toml          # Ctrl+C logs the bots out
```

* `[data] compact` / `server_data` point at the server's `compact.sqlite3` and `app/Data`.
  `[data] baked` is the `--out` directory of `bake` (terrain heights, zones, housing areas, quest
  spheres, world map); without it the bots walk on flat ground and the map has no picture.
* The login server must create unknown accounts (`AutoAccount`). Bot accounts are `<prefix><n>`
  (`server.account_prefix`, default `pb`) with one shared password: `server.password`, else
  `$PLAYERBOTS_PASSWORD`, else a random one that the first run generates and saves to
  `roster.secrets.json` next to the roster (readable by the owner only, git-ignored; never shown
  by the web API, the logs or `Debug`). Keep it together with `roster.json`. A roster that predates
  the file keeps the old public default `playerbots` for its existing accounts, with a warning at
  start-up; change the password of those accounts (and set `server.password`) or start over.
* The web UI is on `http://127.0.0.1:8790/` (`[web] bind`, optional `token` for POST requests;
  while it is bound to a loopback address every request must also carry a loopback `Host`).
* `playerbots run --count 50 --duration-secs 600` overrides the bot count and stops after ten
  minutes.
* Bots are started `bots.ramp_ms` apart; more can be started and stopped from the web UI.

### GM bootstrap

With `bots.gm_bootstrap = true` the bots may use GM chat commands (`/item add self ...`,
`/labor ...`, `/gold add self ...`, `/build ...`, and `/move` for long trips when
`bots.long_travel_teleport_m` is set) to get exactly what a trade run, a trade or a house needs.
The commands only work for accounts with access level 100. Print the SQL that grants it, run it on
the game database and restart the bots:

```sh
playerbots grant-gm-sql --config playerbots.toml     # UPDATE aaemu_game.accounts SET access_level = 100 WHERE username IN ('pb1', ...)
```

The statements name exactly the accounts of the roster (`--game-db` / `--login-db` name the
databases when the server does not use `aaemu_game` / `aaemu_login`); nothing like a name pattern
that a real player's account could match.

Because these accounts are GM accounts, `gm_bootstrap = true` is refused while the bots use the
public default password or none: give them a private one (the generated password does).

Safety of the GM lines:

* Only the bots' own goals send GM commands, and only with `gm_bootstrap` on; chat lines written by
  players or by the LLM can never become commands (see "Safety"). Every plain chat line goes
  through one guard that refuses a command, however it is hidden (leading blanks, NULs, invisible
  or look-alike slashes); the GM path is a separate builder.
* All GM lines go through `Ctx::gm`, which allows only a short list of commands and always
  addresses the bot itself: `/level <own name> N`, `/labor <own name> N`, `/move <own name> x y z`,
  `/item add self ...`, `/gold add self ...`. The server aims some commands at the sender's
  target (or at the sender when it has none), so the target is cleared (`CSChangeTarget(0)`) in the
  same flush before each of them, and after a fight against a character. `/build` is the one
  command that needs a target (the house), and it is sent right after targeting that house.
* `[behavior] trade_with_players = false` (default): a bot trades only with other bots of the
  roster and declines the trade request of any other character, so nobody can farm the goods and
  gold the bots mint for themselves. The option is ignored while `gm_bootstrap` is on.
* `[behavior] portals = true` (default): bots may travel by portal (teleport book, recall points,
  gates); `false` keeps them off portals.
* `[behavior] dungeons = true` (default): bots may form parties and enter instance dungeons (goal
  `dungeon`); `false` keeps them out of both.
* `[behavior] sailing = true` (default): bots may summon boats and sail (goal `sail`); `false` keeps
  them on land.
* `[behavior] navmesh = true` (default): bots walk routes on the navigation mesh baked from the
  client (`nav_main_world.bin`, one more per instance world) instead of straight lines: around
  walls, cliffs and water, along roads, with rounded corners and a slower pace over the last metres.
  Short trips are planned on the bot's own task, long ones by two planner threads; a goal the mesh
  cannot reach, or a planner that answers late, falls back to the straight line. `false` keeps the
  straight lines. Without the baked mesh the option does nothing. Counters (searches inline and
  pooled, latency, fallbacks, stuck events, trips) are in `/api/stats` under `nav` and in the log
  once a minute.

## Chat and the LLM

### Chat without the LLM

Social chat is on by default (`[social]`) and needs no LLM:

* A whisper to a bot is answered with a whisper; a local line that names the bot, or that a real
  player (a character that is not one of the roster) says within `social.reply_radius_m` (30 m),
  is answered in local chat. Of all the bots that overhear an unaddressed line only one answers.
* The reply is a short line from a small table, flavoured by the bot's temperament and chosen so
  that it does not repeat itself, after a human-like pause.
* Bots also chat with each other, rarely: "gg" after a duel, a thanks after a trade, now and then
  a hello to a bot standing close by. By default only while a real player is within 150 m.
* Everything is rate limited per bot (a pause between replies, a burst cap, a cap per
  conversation) and for all bots together (`social.replies_per_min`, `social.chatter_per_min`).
* System and notice lines the server sends (for example the answer to a GM command) are not
  dropped: they are kept in a small ring per bot that goals read.

### Chat and decisions with an LLM (optional)

Any OpenAI-compatible `chat/completions` endpoint works. DeepSeek is the default:

```toml
[llm]
enabled = true
base_url = "https://api.deepseek.com"
model = "deepseek-chat"
api_key_env = "DEEPSEEK_API_KEY"      # the NAME of the environment variable; the key is never in the file
max_requests_per_minute = 30
daily_token_budget = 200000           # prompt + completion tokens per UTC day, all bots together
director = true                       # optional, see below
director_interval_secs = 600
```

```sh
export DEEPSEEK_API_KEY=sk-...        # PowerShell: $env:DEEPSEEK_API_KEY = "sk-..."
playerbots run --config playerbots.toml
```

* **Replies.** The bot builds a persona from its roster entry (name, race, class, temperament, the
  zone it is in) and asks the model for one short chat line in character, with the last four lines
  of its conversation with that player as context. The request runs on its own task; the bot never
  waits for the network. Whispers come back as whispers.
* **Director.** With `llm.director = true` each bot asks, about every `director_interval_secs`
  (jittered), which of the goals it may pick right now to do next, given a short summary of its
  level, health, zone, money, recent goals and surroundings. The answer is applied only if the goal
  is still eligible (not switched off, not cooling down, no GM-only goal without `gm_bootstrap`)
  and never over a goal the operator forced; otherwise, or if the request fails, the bot's own
  scoring decides. One request per bot is in flight at most.
* **Costs are bounded.** All bots share `max_requests_per_minute`, `max_concurrent` and
  `daily_token_budget`; a request over a limit is refused locally and costs nothing, and the bots
  fall back to canned lines and their own scoring. A reply is capped at `max_reply_tokens`. The web
  UI shows requests, failures, throttled requests, tokens used today and left, chat replies and
  director picks. A chat request is a persona prompt of a few hundred tokens plus the last lines of
  the conversation, and its reply is capped at `max_reply_tokens`; watch the numbers for a while
  and set the budget to taste.
* **Safety.** Model output is untrusted. Every reply goes through `pb_llm::sanitize_reply`, which
  rejects (rather than repairs) anything that starts with `/` or a look-alike of it (the server
  would run it as a GM command), contains a link or an address, or gives away that the speaker is
  an AI, and cuts the rest to 120 characters. The send path checks again and refuses any chat line
  whose first visible character is a slash, empty or too long. Other players' words only ever
  appear as chat messages, never in the instructions, and names are reduced to letters and digits
  before they reach a prompt. If the key is missing the add-on is simply off.

## Web UI

The map shows every bot by activity, with houses, duel flags and trade outlets; the list can be
searched; selecting a bot shows its state, quests, extra details, the last chat lines it heard or
said and its event log. Commands per bot: pause/resume, force a goal (quest, grind, duel, trade,
trade run, housing, dungeon, sail, rest, wander, or auto), duel or trade with another bot, say a line, whisper a
line to a character. Global: spawn/despawn bots, pause or resume all. With `[web] token` set, POST
requests need the header `x-playerbots-token`. While the UI is bound to a loopback address, every
route (reads included) refuses a request whose `Host` is not a loopback name (a DNS-rebinding page
in a browser can otherwise read the bots' state); a POST with the right token is exempt, as it
cannot come from such a page. Despawning a bot keeps its slot, account and character: spawning
again reuses the stopped bots first and creates a new roster entry only when none is free.

## Server issue reports

While they play, the bots notice problems with the **server** (not with themselves): a quest that
gives no credit, an NPC or doodad that is not where the data says, a kick or an unexpected
disconnect, a character that dies without the server saying so, a malformed packet, a trade or a
sale that does not pay what it should, a warning in the server's log that names the bot. The
detectors file a report; the hub deduplicates them by *what went wrong* (kind and a stable key, for
example `quest=251 npc=3475`), so the same problem met by fifty bots is one **issue** with a count,
the bots that met it, the first and last time and up to three samples (the first and the latest
two, each with the bot's position, goal, packet trail, server log lines and own log).

What the bots look for (the code is `crates/playerbots/src/bot/watch/`; the judgement of each is a
pure function with tests in `watch/classify.rs`):

* **Requests that go unanswered:** a quest accept or turn-in, a loot window, a skill cast or a GM
  `/move` that gets no answer in 6 s (a cast is answered by `SCSkillStarted`, which the server sends
  only for casts with a casting time and for refusals; an accepted instant skill is answered by
  `SCSkillFired` alone and a plot skill by its `SCPlotEvent`s, which the bot reads from the raw packet;
  the default attacks, skill ids below 5000, repeat by themselves and the server accepts a repeat in
  silence, so they are not tracked). Dropped when the bot died, was teleported or walked away, or
  when the whole connection was quiet (that is a stall, not this).
* **The connection:** a packet the parser rejects (key = opcode, with the first bytes of the packet),
  a kick or an unexpected disconnect (key = phase and reason; the bot's own shutdown and the
  login flow's refusals are not issues), a stall (no packet for 8 s or a ping that took more than
  5 s, key = phase; blamed on the bot when its own loop was starved). One report per outage.
* **Quests:** a kill or a cast the server confirmed whose counter never moved (key = quest and
  component); a quest start the bot's own gates let through that the server refuses (key = quest and
  the server's code); a quest giver, turn-in NPC or quest doodad that is not at any spawn point the
  world data has for it.
* **Combat:** a monster that takes no damage from eight confirmed casts (or says it is immune); a
  skill refused for a reason the bot's own checks rule out (range, mana, cooldown, line of sight and
  moving are the bot's fault and stay local).
* **The bot's life:** hit points 0 without a death packet; a character that logs in dead and is not
  revived; a bot that died and is not resurrected after asking.
* **Money and goods:** a finished trade whose purse or bag moved by something other than the two
  offers; a sold trade pack whose payment mail is empty or cannot be read.

Every report carries the **packet trail**: the last 48 packets sent and received, with their names
and ages (`trail.txt` in the evidence folder).

Every issue has a **suspect** (`server`, `data`, `bot`, `unknown`) and a **severity** (`low`,
`medium`, `high`), set by the detector. An issue is *reportable* when its suspect is not `bot` and

* it is a death without a packet, a malformed packet, a kick or an `ERROR` line in the server log
  about a bot, with high severity, seen **once**; or
* it was seen `min_occurrences` times (default 3), or by `min_bots` different bots (default 2).

Issues the bots suspect themselves for stay in the book and the web API, and never go to the
team's queue by themselves. All issues are listed by `GET /api/issues` and `GET /api/issues/{fp}`
(`pb:` and eight hex digits); `POST /api/issues/{fp}/action` with `{"action":"queue"}`,
`"ignore"`, `"resolve"` or `"reopen"` is the API behind the issue panel of the web UI. The book is kept in
`<dir>/issues.json` (default: `issues` next to `bots.roster`), so counts, states and the rate
limits below survive a restart. At most 500 issues are kept (the ones seen least, longest ago and
not queued go first).

For a reportable issue the bots write an **evidence folder**,
`<evidence_dir>/<YYYYMMDD-HHMMSS>-bot-<slug>/`, with `report.json` (the issue and its samples),
`trail.txt` (the packets before each sample), `server-log.txt` and `bot-log.txt`; it is rewritten
(a few times at most) when later samples arrive.

### The team's issue queue

If the team keeps its server issues in a text file that agents work from, point `queue_file` at it
and the bots **append** one entry per reportable issue:

```toml
[issues]
queue_file = 'C:\path\to\aaserverissues.txt'    # the team's issue file (created if missing)
time_label = "EST"                                # the label after the time in the header
server_log = 'C:\path\to\game-server.log'         # the game server's log, when it runs on this machine
```

An entry is a header line `[YYYY-MM-DD HH:MM:SS <label>] <title> (playerbots)` (local time) and
a few lines of prose (what was observed and expected, how often, which bot was doing what where,
the likely server area as a hint, the server log line at the time, a reminder that the report
comes from an automated client and may be a bot-side misunderstanding, the evidence folder and a
marker `pb:<hex>`). Entries are separated by one blank line and the file uses CRLF line breaks. The
file is only ever opened in append mode and written with one `write` per entry: it is never
rewritten or truncated, so other people and agents can edit it (download, edit, upload) in
between. Agents delete an entry when its fix is done; if the issue happens again 30 minutes or
more after the entry was written, the bots read the file (it is small) and, when the marker is gone,
append a "Recurrence after the entry was removed" entry; while the marker is still there they do
nothing.

`queue_mode` is `auto` (a background task appends reportable issues), `manual` (only the web/API
`queue` action does) or `off`. Rate limits keep the file readable: at most `max_queue_per_hour`
(6) entries an hour and at most `max_queue_per_kind_per_day` (2) *new* entries of one kind of issue
in 24 hours; the others wait and stay visible in the web UI.

### The server log

With `server_log` set (the bots must run on the machine that writes the log, or see it through a
share) the log file is followed once a second: only new lines are read, truncation and rotation are
noticed, colour codes are removed, and the newest 4000 lines (1 MiB) are kept. A report gets the
`WARN` and `ERROR` lines within five seconds of it, plus the lines that name the bot's character,
and a new `WARN` or `ERROR` line that names one of our characters is handed to that bot, which
files a "server log during action" report with its own context (at most 20 lines a second go to
the bots). Lines that are ordinary events the server logs at `WARN` (a quest leaving the journal, a
cast refused for line of sight) are dropped by the bot, see `routine_server_line` in
`bot/watch/classify.rs`. Lines look like `HH:MM:SS [LEVEL] target - message`; other prefixes are tolerated.

### Privacy

A report holds no account, password, token or address of the bot: the bot password is never read
by this code, and nothing in a report carries it. As a second line of defence every text that is
stored, written to an evidence folder, appended to the queue file or served by the web API has
IPv4 addresses other than `127.x` replaced by `x.x.x.x` and the value of `password=`, `token:`,
`secret`, `cookie`, `ticket`, `authorization` and similar pairs replaced by `<redacted>`. Texts
are cut to a bounded length and flattened to one line, so a log line cannot forge an entry. The
evidence folders and `issues.json` contain character names, positions and server log lines: keep
them as private as the roster.

### For detector authors

`crates/playerbots/src/issues/mod.rs` has the tour. In short: build an `IssueReport` with
`IssueReport::new(kind, key, title).expected(..).actual(..).suspect(..).severity(..).area(..).fact(..)`
and file it with `ctx.report_issue(report)` in a goal or `self.file_issue(report)` in the bot
loop; the bot's identity, position, zone, goal and log are attached for you, and so is the packet
trail (`bot.state.watch.trail`, a `TrailRing`: `push(now_ms, dir, level, opcode)` per packet, names
are resolved only by `snapshot(now_ms)`), unless the report brought its own. `.packet_head(&bytes)`
keeps the first 96 bytes of the triggering packet. Report an episode once, not on every tick, and
pick the suspect honestly.

A new detector for a goal or the combat controller leaves a `watch::Finding` (a `Copy` note, ids and
counters only) with `ctx.find(..)`; `Finding::report` makes the report (key, title, suspect,
severity) and `Watch::push` keeps one finding per episode and session. Put the decision (when is it
a finding, whose fault is it) into a pure function in `watch/classify.rs` and test it there. A
request the server must answer is registered with `ctx.expect(..)` (or automatically from the
outgoing packet, see `watch/tracker.rs`).

## Memory and scale

One tokio task per bot, no per-bot reader task and no locks on the hot path. The per-bot structures
are small and bounded (the `per_bot_structures_stay_small` test prints them: the fixed part of a
bot is about 3 KB): at most 1024 units and 2048 useful doodads in view, an event
log of 32 lines, a chat ring of 8, at most four lines for each of three conversations. Chat and
director state is allocated at a bot's first tick (well under 1 KB). The terrain height grid is
memory-mapped and shared by all bots, so only the pages around the bots are resident. The web UI
shows the resident memory of the process and the bytes per online bot; the design target is under
256 KiB of bot-side state per bot. Ticks are adaptive with jitter (250 ms while moving or
fighting, longer when idle) so hundreds of bots do not wake up together.

Measured against a local AAEmu Rust server (debug build, 100 bots, mixed goals, 25 minutes):
about 82 KiB of private memory and 0.22 % of one CPU core per bot (the whole process stayed
around 54 MB at 100 bots, world data included), about 1.2 packets/s sent and 90–100 received per
bot in crowded start areas, no disconnects. Server side, each bot costs about the same as a real
player (≈0.5 MB and 0.5–0.6 % CPU in the game server).

## Repository

| Crate | Role |
| --- | --- |
| `crates/proto` (`aa-proto`) | Wire protocol: framing, login client, game connection, packet builders and parsers. |
| `crates/world` (`aa-world`) | Static world knowledge from `compact.sqlite3`, the server's spawn files and the baked data. |
| `crates/bake` (`aa-bake`) | One-off extraction of terrain, map, zones, housing areas and quest spheres from `game_pak`. |
| `crates/web` (`pb-web`) | Operator web UI (JSON API and a single-page map). |
| `crates/llm` (`pb-llm`) | The optional OpenAI-compatible client with rate limit, concurrency cap and token budget. |
| `crates/playerbots` | The binary: config, roster, bot runtime, goals, coordinator, hub, supervisor, server issue reports. |

Design and data formats are in `docs/`. Run `cargo fmt --all`, `cargo clippy --workspace
--all-targets -- -D warnings` and `cargo test --workspace` before committing. Tests that need the
real game data are `#[ignore]`d; run them with `AA_COMPACT`, `AA_SERVER_DATA` (and `AA_BAKED`)
set: `cargo test -p playerbots -- --ignored`. No test contacts an external host: the LLM tests use
a mock endpoint on 127.0.0.1.

## Credits and license

GPL-3.0-or-later (see `LICENSE`). The behaviour of the bots, their classes and the skill rotation
files in `crates/playerbots/data/rotations/` come from
[NulightJens/ArcheAge-PlayerBots](https://github.com/NulightJens/ArcheAge-PlayerBots)
(GPL-3.0-or-later), a server-side mod for AAEmu; the rotation JSONs are copied unchanged (see
`crates/playerbots/data/rotations/NOTICE`) and run by the rotation engine in
`crates/playerbots/src/bot/rotation/`. The server they play on is
[AAEmu](https://github.com/AAEmu), and the game is ArcheAge by XLGAMES / Trion Worlds; this
project is not affiliated with either.
