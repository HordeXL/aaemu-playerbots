# Operating the bots

Reference for running and tuning aaemu-playerbots: configuration, the goals, the GM bootstrap and
its safety rules, chat and the optional LLM, the web UI. The [README](../README.md) has the
overview and the quick start, [ISSUES.md](ISSUES.md) describes the server issue reports, and
[DESIGN.md](DESIGN.md) explains how the code is built.

* [Configuration](#configuration)
* [Goals and behaviour](#goals-and-behaviour)
* [GM bootstrap](#gm-bootstrap)
* [Chat and the LLM](#chat-and-the-llm)
* [Web UI](#web-ui)
* [Memory and scale](#memory-and-scale)

## Configuration

Everything is set in one TOML file. `playerbots example-config > playerbots.toml` writes the
commented example (`playerbots.example.toml` in the repository); every key is optional, the values
in the example are the defaults, and an unknown key is an error. `playerbots run --config <file>`
reads it (default `playerbots.toml`).

| Section | What it holds |
| --- | --- |
| `[server]` | The login server's public address (`login`), the world to enter (`world_id`), the bot account prefix and the shared password. |
| `[data]` | Paths of the server's `compact.sqlite3` and `app/Data` directory, the baked data directory (`baked`) and the world name. |
| `[bots]` | How many bots, the login ramp, the roster file, the races for new characters, `gm_bootstrap`, tick and speed settings, reconnect back-off. |
| `[behavior]` | The weights of the goals (0 switches a goal off), the personas' spread, the switches `trade_with_players`, `portals`, `dungeons`, `dungeon_level_up`, `sailing` and `navmesh`, and the life-like movement settings `lifelike`, `jump_rate` and `emotes`. |
| `[market]` | Selling: `enabled`, `auction`, `chat_ads`, `sell_to_players`, `meet_at`, `max_ads_per_hour` and `ad_interval_secs` (see [Selling](#selling-market)). |
| `[web]` | The web UI's `bind` address and optional `token`. |
| `[social]` | The chat limits (see [Chat and the LLM](#chat-and-the-llm)). |
| `[llm]` | The optional chat and director add-on. |
| `[issues]` | The server issue reports (see [ISSUES.md](ISSUES.md)). |

A few details:

* `[data] compact` / `server_data` point at the server's `compact.sqlite3` and `app/Data`.
  `[data] baked` is the `--out` directory of `bake` (terrain heights, navigation meshes, zones,
  housing areas, quest spheres, world map); without it the bots walk straight lines on flat ground
  and the map has no picture.
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

## Goals and behaviour

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
| **Dungeon** | Bots that like a fight ask the coordinator for a party of three (same alliance, a level the dungeon accepts; with `dungeon_level_up` and GM rights a lower level is raised). The leader invites, the party walks to the entrance doodad, goes in one by one, clears the dungeon along a route of its hostile spawns (the leader picks targets, the members follow and assist, everybody rests when low), uses the exit doodad and disbands. The server's refusals are typed and answered (a dungeon it turned away is left alone, the three entries per four hours are counted); what the server does that the data contradicts is filed as an issue. |
| **Sail** | Summons a boat from a scroll at a beach, sails it 1 to 3 km over the sea (a crossing to another beach or a loop) with a heading controller that finds the wheel's direction itself, then leaves the boat, despawns it and swims ashore. Sea only (no lakes or rivers). Needs a boat scroll in the bag or `gm_bootstrap`. See [DESIGN.md](DESIGN.md#sailing-goal-sail). |
| **Rest / Wander** | Stand still until hit points and mana recover; stroll around. |
| **Social chat** | Answers whispers and nearby players (canned lines, or an LLM in character) and chats a little with other bots (see [Chat and the LLM](#chat-and-the-llm)). A background duty, not a goal. |
| **LLM director** (optional) | Now and then asks the LLM which of the currently eligible goals to do next (see [Chat and the LLM](#chat-and-the-llm)). |

Bots survive death (they request the resurrection), disconnects (reconnect with backoff), name
rejections (a new name is picked) and restarts (the roster remembers accounts and characters).
Names, races, classes and temperaments are stored in `roster.json`: keep it.

Housing and trade runs need materials, labor, tax certificates and gold that a fresh character does
not have; see [GM bootstrap](#gm-bootstrap). Without it those goals only work when the bag already holds what
they need, and the LLM director never offers them.

## GM bootstrap

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
  players or by the LLM can never become commands (see [Chat and the LLM](#chat-and-the-llm), "Safety"). Every plain chat line goes
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
* `[behavior] dungeon_level_up = false` (default): with `bots.gm_bootstrap`, `true` lets a bot below a
  dungeon's level range raise its level to the range's minimum with the GM `/level` command
  (logged); `false` keeps dungeons to the bots whose level fits.
* `[behavior] sailing = true` (default): bots may summon boats and sail (goal `sail`); `false` keeps
  them on land.
* `[behavior] lifelike = true` (default): bots move like people (jumps, emotes, idle mannerisms);
  `false` keeps them walking and standing still. `jump_rate = 1.0` scales how often they jump (0 =
  never) and `emotes = true` lets them use emotes.
* `[market] sell_to_players = false` (default): bots sell only to other bots and the auction house;
  `true` lets them also sell to real players, and is ignored while `gm_bootstrap` is on (same rule
  as `trade_with_players`).
* `[behavior] navmesh = true` (default): bots walk routes on the navigation mesh baked from the
  client (`nav_main_world.bin`, one more per instance world) instead of straight lines: around
  walls, cliffs and water, along roads, with rounded corners and a slower pace over the last metres.
  Short trips are planned on the bot's own task, long ones by two planner threads; a goal the mesh
  cannot reach, or a planner that answers late, falls back to the straight line. `false` keeps the
  straight lines. Without the baked mesh the option does nothing. Counters (searches inline and
  pooled, latency, fallbacks, stuck events, trips) are in `/api/stats` under `nav` and in the log
  once a minute.

## Selling (`[market]`)

The market features let bots sell what they gather: the goal `auction` lists goods at the auction
house, and the trade-chat market advertises them, answers buyers and meets them (the goal `market`
does the meeting). `[market]` is their configuration:

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | `true` | Master switch: `false` and no bot lists, advertises or sells anything. |
| `auction` | `true` | Bots list surplus goods at the auction house. |
| `chat_ads` | `true` | Bots advertise surplus goods in trade chat. |
| `sell_to_players` | `false` | Bots sell to real players in a trade; never while `bots.gm_bootstrap` is on. |
| `meet_at` | `"auto"` | Where a buyer is met: `"auto"` (Mirage Isle when the bot can get there, else the town), `"mirage"` or `"town"`. |
| `max_ads_per_hour` | `12` | All bots together post at most this many advertisements per hour (0 = none). |
| `ad_interval_secs` | `900` | One bot posts one at most this often (at least 60). |

**What a bot may sell.** Never "what is in the bag" but what it came by honestly. Every item change
the server sends names its reason (`ItemTaskType`); the bot's provenance ledger
(`bot/market/ledger.rs`) counts a gain only for loot (9, 10, 35), a picked-up craft product (29),
quest rewards and supplies (37, 38), gathering skill effects (42, 43) and vendor purchases (54).
GM-made goods (11), trades, mail and auction returns never count. Whatever leaves the bag, for any
reason, is taken off the ledger first, so the ledger errs on the side of selling less; it holds at
most 64 item kinds per bot and is forgotten when the connection is lost. Of what is left, only
items that are sellable and not bound (`ItemFacts::tradable`) are offered, and a reserve of 10 is
kept of items the bot may use itself (anything that casts a skill when used, anything a quest asks
to gather).

### Selling in trade chat

The chat market is a duty every bot runs next to its goal, plus the goal `market` for the meeting.

* **Ads.** A bot with clean surplus posts a `WTS` line in the trade channel; one that needs goods
  posts a `WTB` line. Wording varies, prices come from the vendor values of the item (a little
  over half way between what a vendor pays and asks), lines are plain text of at most 200
  characters with no item links. A bot posts only when somebody could read it (other bots or a
  player in the same zone group's trade chat), at most every `ad_interval_secs` (with jitter),
  and all bots together at most `max_ads_per_hour` ads and 14 market lines per minute.
* **Talk.** A small grammar reads "how much", "price", "i'll take", "x20", "30s", "1g", "deal",
  "ok", "no", "too much", "meet", "where". The seller quotes, counters once and never goes under
  its floor; the buyer haggles once and never pays over its ceiling. Replies go out the way the
  other side spoke (whisper for whisper, the trade channel addressed by name for channel lines),
  which also serves characters of the other faction, who cannot be whispered (error 437). Bots
  buy from and sell to each other's ads. Nothing a bot says gives away that it is a bot.
* **Meeting.** `meet_at = "auto"`: the same faction near the same town meets at that town's
  auctioneer; another faction, far apart, or a buyer nobody can place meets on Mirage Isle. The
  bot walks to the nearest portal doodad (template 4894, next to the auctioneers), casts it, waits
  at the mailbox, trades, and leaves through the exit doodad (4895). It waits four minutes for
  the buyer and drops a deal after twelve.
* **Safe trades.** Every trade made from chat has a deal: who, what the bot gives, what it must
  receive. The partner must be the agreed character; money must be at least the price and the
  goods exactly the agreed ones; the bot's own offer must be unchanged. The server unlocks both
  sides on any change, so the bot checks again after every unlock before it locks, and cancels on
  a mismatch (one copper for the goods, a swapped item, an offer changed after the lock, the wrong
  character) or on a timeout. Requests from anybody else during a meeting are declined.
* **Economy safety.** Only the clean ledger is sold, never GM-made goods, trade, mail or auction
  returns. Real players are served only with `sell_to_players = true`, never while
  `bots.gm_bootstrap` is on. Everything is off with `enabled = false` or `chat_ads = false`.
* **Issue detectors** (see Server issue reports): a portal doodad that is not in view 20 s after
  the bot reached its spawn point (the data places it, the server does not show it), a portal
  cast the server accepted that never loaded the island within 30 s, an exit cast that leaves the
  bot on the island, a trade both
  sides confirmed that moved nothing, and refused whispers (error 437 or 265, reported only as
  information, the bot being the suspect).

Force the goal from the web UI to make a bot post an ad at once and listen for buyers for four
minutes. The stand-in "need" of a WTB bot is one of the shared trade goods it holds hardly any of.

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
trade run, housing, dungeon, sail, auction, market, rest, wander, or auto), duel or trade with another bot, say a line, whisper a
line to a character. Global: spawn/despawn bots, pause or resume all. With `[web] token` set, POST
requests need the header `x-playerbots-token`. While the UI is bound to a loopback address, every
route (reads included) refuses a request whose `Host` is not a loopback name (a DNS-rebinding page
in a browser can otherwise read the bots' state); a POST with the right token is exempt, as it
cannot come from such a page. Despawning a bot keeps its slot, account and character: spawning
again reuses the stopped bots first and creates a new roster entry only when none is free.

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

Measured numbers are in the [README](../README.md#memory-and-scale).
