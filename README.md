# aaemu-playerbots

Headless player bots for the [AAEmu Rust server](https://github.com/AAEmu/AAEmu-1.2-Rust)
(ArcheAge 1.2, r208022). Every bot is a network client: it logs in through the login server,
creates or selects a character and plays over the normal client protocol, so the server needs no
changes. Hundreds of bots run in one process (one tokio task per bot) and show up on a live map in
a small web UI.

Today the bots log in, create characters, wander, rest, fight and loot monsters, survive death and
disconnects, and can be paused, steered and watched from the web UI. Quests, duels, player trade,
trade runs and housing are planned and are stubs in this build (see `docs/DESIGN.md`).

## Quick start

```sh
cargo build --release -p playerbots
playerbots example-config > playerbots.toml     # edit the paths in [data] and the login address
playerbots bake --pak <client>/game_pak --out <dir> --compact <compact.sqlite3>   # once, optional
playerbots run --config playerbots.toml          # Ctrl+C logs the bots out
```

* `[data] compact` / `server_data` point at the server's `compact.sqlite3` and `app/Data`.
  `[data] baked` is the `--out` directory of `bake` (terrain heights, zones, world map); without it
  the bots walk on flat ground and the map has no picture.
* The web UI is on `http://127.0.0.1:8790/` (`[web] bind`, optional `token`).
* `playerbots run --count 50 --duration-secs 600` overrides the bot count and stops after ten
  minutes.
* `playerbots grant-gm-sql` prints the SQL that gives the bot accounts GM access, needed only for
  `bots.gm_bootstrap = true` (bots then use `/move` and friends).
* Bot accounts are `<prefix><n>` with one shared password (`server.password`, else
  `$PLAYERBOTS_PASSWORD`, else `playerbots`); the login server must create unknown accounts
  (`AutoAccount`). Names, races and classes are stored in `roster.json`: keep it.

Design and data formats are in `docs/`. Run `cargo fmt --all`, `cargo clippy --workspace
--all-targets -- -D warnings` and `cargo test --workspace` before committing. Tests that need the
real game data are `#[ignore]`d; run them with `AA_COMPACT`, `AA_SERVER_DATA` (and `AA_BAKED`)
set: `cargo test -p playerbots -- --ignored`.

## Credits and license

GPL-3.0-or-later (see `LICENSE`). The bot classes and the skill rotation files in
`crates/playerbots/data/rotations/` come from
[NulightJens/ArcheAge-PlayerBots](https://github.com/NulightJens/ArcheAge-PlayerBots)
(GPL-3.0-or-later); the rotation JSONs are copied unchanged (see
`crates/playerbots/data/rotations/NOTICE`) and are not interpreted by the runtime yet.
