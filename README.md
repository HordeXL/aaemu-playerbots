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
trade run, housing, rest, wander, or auto), duel or trade with another bot, say a line, whisper a
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

## Repository

| Crate | Role |
| --- | --- |
| `crates/proto` (`aa-proto`) | Wire protocol: framing, login client, game connection, packet builders and parsers. |
| `crates/world` (`aa-world`) | Static world knowledge from `compact.sqlite3`, the server's spawn files and the baked data. |
| `crates/bake` (`aa-bake`) | One-off extraction of terrain, map, zones, housing areas and quest spheres from `game_pak`. |
| `crates/web` (`pb-web`) | Operator web UI (JSON API and a single-page map). |
| `crates/llm` (`pb-llm`) | The optional OpenAI-compatible client with rate limit, concurrency cap and token budget. |
| `crates/playerbots` | The binary: config, roster, bot runtime, goals, coordinator, hub, supervisor. |

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
