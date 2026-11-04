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
| `[guild]` | Guilds, off by default: `enabled`, `found`, `min_level`, `party_size`, `max_guilds` and `accept_player_invites` (see [Switched-off features](#switched-off-features)). |
| `[pvp]` | Player versus player, off by default: `enabled`, `defend`, `attack_bots`, `attack_players`, `gank`, `warzone`, `never_names`, `only_names`, `max_squad` and `retreat_hp_pct`. |
| `[events]` | World events such as rifts, off by default: `enabled`, `max_bots_per_event`, `max_share`, `join_delay_secs` and `avoid_war_zones`. |
| `[party]` | Bots and the parties of real players, off by default: `join_player_parties`, `chat_commands` and `follow_into_dungeons`. |
| `[population]` | The web UI's population director, off by default and a dry run when on: `enabled`, `dry_run`, `max_spawn_per_min` and `min_action_gap_secs`. |
| `[farm]` | Farming, off by default: `enabled` and `max_plots`. |
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
| **Player trade** | The coordinator pairs two bots; they meet, open the trade window, put up goods and/or money, lock and confirm. A trade request from a real player is accepted when the bot is idle. A bot puts up only tradable items it does not hold back for a quest (see [Selling](#selling-market)). |
| **Housing** | Picks a design and a free spot in a housing area, quotes the tax, places the house, builds it and later "goes home" to rest there. |
| **Auction** | Walks to the nearest auctioneer, lists surplus goods, buys a bargain and goes home (see [Auction house](#auction-house)). Needs `[market] enabled` and `auction`. |
| **Trade run** | Crafts a trade pack at a specialty workbench, carries it to a specialty NPC that buys it, sells it and collects the payment mail. |
| **Dungeon** | Bots that like a fight ask the coordinator for a party of three (same alliance, a level the dungeon accepts; with `dungeon_level_up` and GM rights a lower level is raised). The leader invites, the party walks to the entrance doodad, goes in one by one, clears the dungeon along a route of its hostile spawns (the leader picks targets, the members follow and assist, everybody rests when low), uses the exit doodad and disbands. The server's refusals are typed and answered (a dungeon it turned away is left alone, the three entries per four hours are counted); what the server does that the data contradicts is filed as an issue. |
| **Sail** | Summons a boat from a scroll at a beach, sails it 1 to 3 km over the sea (a crossing to another beach or a loop) with a heading controller that finds the wheel's direction itself, then leaves the boat, despawns it and swims ashore. Sea only (no lakes or rivers). Needs a boat scroll in the bag or `gm_bootstrap`. See [DESIGN.md](DESIGN.md#sailing-goal-sail). |
| **Guild, Gank, Warzone, Event, Companion, Farm** | Reserved: found or join a guild, ambush trade-pack carriers, fight in zones at war, join a world event, follow a real player's party, plant and harvest crops. The goals exist (the web UI can name them) but are not implemented yet: a bot that is told to do one switches it off. Their settings are in [Switched-off features](#switched-off-features). |
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
  never) and `emotes = true` lets them use emotes. It only shows when a real player (a character that
  is not one of this process's bots) is within 150 m, and never while a bot fights, trades, sails or is
  in an instance. The server logs every emote at WARN (`express_text - ExpressEmotion`); a
  process-wide allowance keeps that to a few lines a minute however many bots run. To see what the
  bots do: `RUST_LOG=info,pb_life=debug` logs every jump and emote, and `pb_move=trace` logs the
  movement packets other characters send, as the server relays them. Details in
  [DESIGN.md](DESIGN.md#life-like-movement-botmannerrs).
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

## Switched-off features

These sections are all off by default; `enabled = false` (or the `false` default of the switch named
below) keeps the bots exactly as they were without them. The goals they belong to are reserved and
not implemented yet (see [Goals and behaviour](#goals-and-behaviour)); the settings are read and
checked now so the features can be added without changing the file.

| Section | Key | Default | Meaning |
| --- | --- | --- | --- |
| `[guild]` | `enabled` | `false` | Master switch of the guild features. |
| | `found` | `true` | Bots may found guilds (otherwise they only join). |
| | `min_level` | `5` | A bot founds or joins a guild from this level on. |
| | `party_size` | `5` | Bots that found a guild together (1 to 50). |
| | `max_guilds` | `3` | This process founds at most this many guilds (0 = none). |
| | `accept_player_invites` | `false` | Bots accept the guild invitations of real players. |
| `[pvp]` | `enabled` | `false` | Master switch of the pvp features. |
| | `defend` | `true` | A bot fights back when a player attacks it. |
| | `attack_bots` | `false` | Bots of hostile factions attack each other. |
| | `attack_players` | `false` | Bots attack real players; never while `bots.gm_bootstrap` is on (what a GM-bootstrapped bot holds may be minted), like `sell_to_players`. |
| | `gank` | `false` | Bots ambush trade-pack carriers (goal `gank`). |
| | `warzone` | `false` | Bots fight in zones at war (goal `warzone`). |
| | `never_names` | `[]` | Character names that are never attacked (game masters, friends). |
| | `only_names` | `[]` | When not empty: the only real players that may be attacked. |
| | `max_squad` | `3` | Most bots that attack together (1 to 50). |
| | `retreat_hp_pct` | `35` | A bot below this many percent of its hit points breaks off a fight (0 to 100). |
| `[events]` | `enabled` | `false` | Master switch of the event features (goal `event`). |
| | `max_bots_per_event` | `6` | At most this many bots join one event. |
| | `max_share` | `0.25` | At most this share (0 to 1) of all bots takes part in events at once. |
| | `join_delay_secs` | `[10, 120]` | A bot waits a random number of seconds in `[low, high]` before it joins. |
| | `avoid_war_zones` | `true` | Bots stay out of zones at war. |
| `[party]` | `join_player_parties` | `false` | Bots join the parties of real players when invited (goal `companion`). |
| | `chat_commands` | `false` | Bots obey party chat commands of the player who leads the party. |
| | `follow_into_dungeons` | `false` | A bot in a player's party follows it into instance dungeons. |
| `[population]` | `enabled` | `false` | Master switch of the population director (see [World metrics and the population director](#world-metrics-and-the-population-director), which lists every key). |
| `[farm]` | `enabled` | `false` | Master switch of the farming features (goal `farm`). |
| | `max_plots` | `5` | One bot tends at most this many plots. |

The process warns at start when `pvp.attack_players` is set together with `bots.gm_bootstrap`.

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
items that are tradable (`ItemFacts::tradable`) are offered, and the bot keeps back a part of them.

* **Tradable** means sellable (a vendor takes it), unbound (`bind_id` 1) and not a quest item. A
  quest item is one in the quest item category, one that only drops during a quest, or one that a
  quest tells the bot to take (an item that starts a quest, a quest step's obtained or supplied
  items; the reward step's items are ordinary). The materials a quest merely asks the bot to
  gather or use (hay bales, seeds) are not quest items. An item the table does not know is not
  tradable. The same test decides everything below: a bot never sells, asks for, buys or hands
  over an item that is not tradable.
* **Kept back** per item kind is the larger of two numbers: a reserve of 10 of items the bot may
  use itself (anything that casts a skill when used, anything a quest asks to gather), and what the
  bot's active quests still ask of it (the gather and use counts of their objectives, summed).
  An item a quest needs in full is never offered, listed, or put up in a trade, and a deal agreed
  in chat is dropped when a quest taken in the meantime needs the goods.

### Auction house

With `[market] enabled` and `auction` a bot visits an auctioneer at most about every 45 minutes,
when it has goods worth the trip (or gold to spend) and the auctioneer is within 3.5 km on the
walkable mesh. The nearest reachable auctioneer comes from the world data's NPC spawns. A visit is
a state machine that lasts a few minutes:

1. **Walk** to the auctioneer and wait until it is in view (an auctioneer that is not at its spawn
   is filed as an issue). An auctioneer the bot cannot reach, or died on the way to, is left alone
   for half an hour.
2. **Survey** the first pages of the house and the character's own lots.
3. **Price and post.** For each honest surplus stack (see [Selling](#selling-market)) the bot looks
   at the five cheapest buyouts of the item, takes their median per item as the going rate and asks
   95 to 99 % of it; with no market it asks between the vendor price and the item's base price. The
   fee (1 % of the buyout for 6 hours up to 4 % for the 48 hours the bot chooses, paid in cash) is
   not refunded and the seller gets 90 % of the price by mail, so a stack is listed only when that
   net beats the vendor refund by 8 %. Up to four whole stacks are listed per visit (the bot cannot
   split stacks), a bot that cannot pay the expected fees does not go, and the start price is 90 %
   of the buyout.
4. **Cancel and relist.** Now and then (35 %) a lot older than 90 minutes that has been undercut
   by more than 20 % is looked at again and cancelled; the item comes back by mail and is priced
   again.
5. **Browse and buy.** Only when `bots.gm_bootstrap` is off: with gold above 2,000 copper the bot
   looks at the cheapest lots (the first pages) and buys out up to two of them: a lot that costs at
   most 75 % of what a vendor would pay for it, or a consumable the bot can use itself (it casts a
   skill and the item's level fits) that costs no more than a shop asks. A lot of an item that is
   not tradable (a quest item, a bound or an unsellable one) is never bought, however cheap: no
   vendor would take it back. It spends at most a quarter of its money (50,000 copper at most) per
   visit.
6. **Linger** a few seconds, then leave.

The results arrive as mail (types 13 cancelled lot, 14 sale, 15 expired lot or outbid refund, 16
won lot). A background duty lists the mailbox every five minutes and after a post, reads each new
auction mail and takes its money or item; mails stay in the box after taking, so the bot remembers
the ones it settled. Memory is bounded per bot (24 price notes, 16 own lots, 8 skips, 4 pending wins).

Reported as issues: a post that took the item and fee without `SCAuctionPosted`, a confirmed lot
missing from the own lots, a search that gets no answer next to the auctioneer, a buyout that took
the money without `SCAuctionBid` or never delivered the item, an own lot that vanished with no
result mail, an auction mail that is empty or cannot be read, and any error code that the rules do
not explain (241 cannot pay and 388 too far are handled). The log line at the end of a visit
totals the bot's visits, lots posted, fees, sales, purchases and timeouts.

### Selling in trade chat

The chat market is a duty every bot runs next to its goal, plus the goal `market` for the meeting.

* **Ads.** A bot with clean surplus posts a `WTS` line in the trade channel; one that needs goods
  posts a `WTB` line (only for tradable goods). Item names that a line is read against are those
  of tradable items, so a request for a quest or bound item finds no match and gets no answer.
  Wording varies, prices come from the vendor values of the item (a little
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
  returns, and only tradable items beyond what the bot's quests need. Real players are served only with `sell_to_players = true`, never while
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
trade run, housing, dungeon, sail, auction, market, guild, gank, warzone, event, companion, farm,
rest, wander, or auto), duel or trade with another bot, say a line, whisper a
line to a character. Global: spawn/despawn bots, pause or resume all. With `[web] token` set, POST
requests need the header `x-playerbots-token`. While the UI is bound to a loopback address, every
route (reads included) refuses a request whose `Host` is not a loopback name (a DNS-rebinding page
in a browser can otherwise read the bots' state); a POST with the right token is exempt, as it
cannot come from such a page. Despawning a bot keeps its slot, account and character: spawning
again reuses the stopped bots first and creates a new roster entry only when none is free.

The World, Economy and Director views, the heat overlay and the director are described in
[World metrics and the population director](#world-metrics-and-the-population-director).

## World metrics and the population director

The side panel of the web UI has three views that look at the world instead of at single bots, and
a director that can log bots in and out because of what they show. The metrics are always on; only
the director is switched by `[population] enabled` (and it starts as a dry run).

### What the bots can see

A bot sees the characters within about 128 m. Every bot hands the characters in view that are not
one of ours to the **census**: character id, name, level, side, continent, zone and position. A
player counts for 60 seconds after the last bot saw it, and the census holds at most 2048 players.
So the census is a **lower bound** of the players online: a player in a corner of the map that no
bot passes is not in it. The World view shows the coverage when it can.

The side of a character is the mother faction of its faction (Nuia 148, Haranya 149); the continent
is the zone group of the zone it stands in (west, east, Auroria, sea). A bot's snapshot carries both
(`mother_faction`, `alliance`, `continent`, `zone_key`); before a bot is in the world, its side is
the side of its race.

### The views

* **World.** Tiles: players seen, bots online, players on the server, coverage, watched area. A
  table of continent by side (players seen / bots online), with a `+` in the home continent of each
  side (starts bots of that side) and a `-` wherever bots are (stops idle ones). A form to start or
  stop bots by side, race and continent. Busiest zones, the players in view (name, level, side,
  zone, age), what the online bots do (their goals) and what the game server says about itself.
  "Players on the server" and "unlocated" (players the server counts that no bot sees) need the
  server API, see below.
* **Heat** (checkbox above the map): 512 m squares with players (red) and bots (blue).
* **Economy.** The scarcest items first, with the scarcity bar, lots on the auction house, the
  market price against the vendor price, the price change over 30 minutes and the buy / sell lines
  in trade chat. `Grow` makes an item a *wish*; wishes are listed below the table with their time
  left. Hover a row for the reasons.
* **Director.** Mode (off, dry run, live, holding), the kill switch, the dry-run switch, the two
  sides (players, bots, the director's own bots, target), process health and its limits, the policy
  numbers, goal weights and the log of everything the director did or would do.

`#tab=world`, `#tab=economy` and `#tab=director` open a view from a link (`&heat=1` turns the heat on).
The raw data is `GET /api/world`, `GET /api/economy` and `GET /api/director`; the director's policy is
changed with `POST /api/director/policy` (a JSON object with the fields to change, e.g.
`{"dry_run":false}`), and `POST /api/command` takes `spawn_where` and `despawn_where`
(`{"type":"spawn_where","count":3,"filter":{"alliance":"nuia","race":"elf","continent":"west"}}`,
every filter key optional). These POSTs need the token like every other one.

**Filtered spawn and despawn.** A spawn picks free bots of the roster that match (the race decides
the side and the continent a bot starts on: Nuian and Elf are Nuia and start in the west, Hariharan
and Ferre are Haranya and start in the east) and grows the roster with matching races from
`bots.races` when there are too few. A despawn matches by where the bot is now (its side and the
continent it stands on) and picks the most harmless bots first: ones that stand, rest or wander,
then ones that walk to a quest or grind, last ones that carry a trade pack. It never stops a bot
that is fighting, dead, logging in, trading, dueling, in a dungeon, on a boat, at a market or
auction, in a guild or event task, a warzone or a gank (those goals finish first). Bots that never
logged in leave the start queue first.

### Economy and scarcity

The board follows up to 1500 items from three kinds of evidence, all bounded:

* the **price notes** of the bots (what they learned at the auction house: the market quote, how
  many lots they saw, five at most); every bot hands in its fresh notes every ten seconds, and the
  board keeps the latest note of six bots per item and a short price history;
* **trade chat**: a shout, trade or group-finding line that offers or asks for an item counts as a
  sell or a buy line for ten minutes. All bots near a speaker hear the same line, so it is counted
  once. A real player's line counts in full, a bot's a quarter (the bots' own advertisements must
  not drive the scarcity they would react to);
* the **server's auction list** (with `server_api` and its key): the exact number of lots and the
  median price per item; when it is fresh (ten minutes) it replaces the bots' guess at the supply.

The scarcity score runs from 0 (plenty) to 1 and adds four parts: few lots (`0.45 / (1 + lots / 2)`,
none for sale is the full 0.45), a price far above the vendor price (up to 0.25 at four times), a
rising price (up to 0.15 at +50 % in half an hour) and more people asking than offering in chat (up to
0.15). The farming goal reads it through `population::economy::ScarcityView` (`scarcity`,
`most_scarce`, `wished`), which `hub.pop` implements.

### The director

Once a second the director looks at the players per side (the census) and the bots per side and
decides, by the rule **mirror**, how many bots each side should have:

```
target[side] = min(per_side_cap, max(min_per_side, ceil(mirror_pct / 100 * players[other side]) - players[side]))
```

When the east is heavy with players, the west gets bots until it has as many characters (players
plus bots) as the east, within the caps. A side that is short gets bots, up to `step` per action,
the shortest side first, and never above `max_online` bots in all. A side that has more than
`hysteresis` bots above its target loses the whole surplus, `step` per action, but **only bots the
director started itself** that have been up for 3 minutes and are safe to stop (see above): bots you
started by hand or at start-up are never touched. The dead band keeps a player at the edge of a
bot's view from making bots come and go.

**Safety.**

* It is **off** by default (`enabled = false`), and with `dry_run = true` (also the default) it only
  writes down what it would do, to the log of the Director view and the process log (`pb_pop`). The
  switches always start as `[population]` says, whatever the last run ended with (`policy_file` keeps
  only the numbers, wishes and goal weights).
* The kill switch of the Director view turns the whole director off at once; going live asks for
  confirmation. If the configuration has `enabled = false` it cannot be switched on from the UI.
* One action at a time, `min_action_gap_secs` apart, at most `max_spawn_per_min` bots a minute
  (both are hard limits from the configuration that the UI cannot lift), at most `step` bots per
  action, at most `max_online` bots and `per_side_cap` per side (every count is also bounded by the
  roster limit of 5000 bots).
* **Back-off**: while the bot process is over a limit it starts no bots (it may still stop some). The
  limits: memory, packets per second from the server, failed or short sessions in the last five
  minutes, open server issues. The reason is shown ("on hold: ...") and logged once.
* Every action, every refusal and every policy change is in the log with its reason. A dry run keeps
  the pace of a live run and does not repeat itself.

**Goal weights and wishes.** The Director view can scale the score of each goal for every bot
(0.1 to 3; blank is 1), applied while the director is live. Wishes (from the Economy view, or the
scarcest items when `auto_boost` is above 0) are recorded for the farming goal; while the director is
live and any wish exists, the `farm` goal is scored 1.5 times higher unless you set a weight for it.
Nothing grows crops yet, so this changes nothing until the farming goal exists.

**`[population]` keys.**

| Key | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Master switch of the director (the metrics are always on). |
| `dry_run` | `true` | The director only logs what it would do. |
| `max_spawn_per_min` | `10` | Hard limit: bots it starts per minute (0 = none). |
| `min_action_gap_secs` | `60` | Hard limit: seconds between two of its actions (at least 1). |
| `max_online` | `50` | Start values of the policy (editable live in the Director view): bots online in all. |
| `min_per_side` | `0` | Each side keeps at least this many bots. |
| `mirror_pct` | `100` | Characters per 100 players on the other side (0 to 1000). |
| `per_side_cap` | `20` | No side gets more bots than this from the director. |
| `step` | `3` | The most bots one action starts or stops (1 to 1000). |
| `hysteresis` | `2` | Bots above the target that a side keeps. |
| `despawn` | `true` | The director may stop the bots it started. |
| `backoff_rss_mb` | `3072` | Start no bots above this process memory (0 = ignore). |
| `backoff_packets_in` | `30000` | ... above this many packets per second (0 = ignore). |
| `backoff_login_failures` | `8` | ... after this many failed or short sessions in five minutes. |
| `backoff_open_issues` | `50` | ... with this many open server issues. |
| `policy_file` | unset | Keeps the numbers, wishes and goal weights between runs. |
| `server_api` | `""` | The game server's web API (`http://host:port`); empty = not used. |
| `server_api_key_env` | `""` | Name of the environment variable that holds the API's bearer key. |
| `server_api_key_file` | unset | A file whose first line is the key (used when the variable is unset). |
| `server_poll_secs` | `30` | Seconds between two polls (at least 5). |

The operator-only numbers `auto_boost` (automatic wishes, 0 to 16, default 0) and `auto_boost_min`
(scarcity from which an item becomes a wish, default 0.7) are set in the Director view.

### The server API (optional)

With `server_api` set, a background task polls the game server's own web API every
`server_poll_secs`: `GET /status` (public: uptime, players online, job counts), and with the key
`GET /api/world/logged-characters` (the exact list of online characters: the ones that are not our
bots are the real players, so the World view can say how many no bot sees) and, every fourth poll,
`GET /api/auction/list` (the exact lots, for the economy board). Without the key only `/status` is
used, and its `Players online` count (which includes every bot of every process) has our own bots
in the world taken off it ("approx." in the tile). The key comes from the environment variable named
in `server_api_key_env` or the first line of `server_api_key_file`; it is held in memory only, sent
only as the `Authorization` header and never logged, shown or written into an error text. The poll is
read-only and fails soft: an unreachable server is a line in the World view. Responses are cut at
16 MB, redirects are not followed and the request times out after 5 seconds.

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
