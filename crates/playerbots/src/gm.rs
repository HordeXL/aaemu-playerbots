//! GM access: the commands the bots may send and the SQL that grants them access level 100.
//!
//! # Commands and who they hit
//!
//! Bots only use GM chat commands (`/move`, `/item add`, ...) when `bots.gm_bootstrap` is on, and
//! the server only runs them for access level 100. Most of them do not act on the sender: the
//! server resolves the victim of `/level`, `/labor`, `/gold` and `/move` with `GetTargetOrSelf`:
//! first an online character whose *name* equals the first argument, then the sender's current
//! *target* when that is a player, and only then the sender. A bot that dueled a real player still
//! has that player as its target, so a bare `/move 1 2 3` could teleport a real player. Every GM
//! line therefore goes through [`plan`], which
//!
//! * only lets the allowlisted commands through, in the shapes the goals use,
//! * rewrites the target-or-self commands into their explicit form (`/level <own name> 36`;
//!   `/item add self ...` and `/gold add self ...` are explicit already) and
//! * asks the caller to clear the server-side target in front of the command
//!   ([`Pre::ClearTarget`]), so even a failed name lookup ends on the bot itself.
//!
//! `/build` acts on the *targeted house*: it has its own entry point ([`plan_build`]) that makes
//! the caller target the bot's house right before the command (`Ctx::gm_build`).
//!
//! # Granting access
//!
//! The login server creates the accounts (and the game server their `accounts` rows) on the first
//! login, so run [`grant_sql`]'s statements after the bots have logged in once. They name the
//! roster's accounts exactly (`username IN (...)`), never a pattern: anybody can register
//! `pb99999` on a server with `AutoAccount`. The statements name the two MySQL databases of the
//! server (`aaemu_game` and `aaemu_login` by default; an isolated test server uses its own).

/// Access level the server's GM commands need.
pub const GM_LEVEL: u32 = 100;
/// The game server's database unless the server is set up differently.
pub const DEFAULT_GAME_DB: &str = "aaemu_game";
/// The login server's database unless the server is set up differently.
pub const DEFAULT_LOGIN_DB: &str = "aaemu_login";

/// What a GM line needs in front of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pre {
    /// Nothing: the line names the bot itself (`self`).
    None,
    /// The server-side target must be cleared first (`CSChangeTarget(0)`); the line names the bot.
    ClearTarget,
    /// The line acts on the targeted house: only [`plan_build`] hands these out.
    HouseTarget,
}

/// A GM line as it may go on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmWire {
    /// The chat line, starting with `/`.
    pub line: String,
    /// What to send in front of it.
    pub pre: Pre,
}

impl GmWire {
    /// Whether the line can only affect the bot itself (or its own house): it says `self` or names
    /// the bot, with the target cleared in front where the server would look at the target.
    pub fn is_self_only(&self, own_name: &str) -> bool {
        let t: Vec<&str> = self.line.split_whitespace().collect();
        match (t.first().copied(), self.pre) {
            (Some("/item" | "/gold"), Pre::None) => {
                t.get(1) == Some(&"add") && t.get(2) == Some(&"self")
            }
            (Some("/level" | "/labor" | "/move"), Pre::ClearTarget) => {
                t.get(1).is_some_and(|n| n.eq_ignore_ascii_case(own_name))
            }
            (Some("/build"), Pre::HouseTarget) => true,
            _ => false,
        }
    }
}

/// Why a GM line was not planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GmRefused {
    /// The line does not start with `/` or has control characters.
    NotACommand,
    /// The command is not one the bots send.
    UnknownCommand(String),
    /// The arguments are not in the shape the goals use.
    BadArguments(&'static str),
    /// The bot's own name cannot stand as a command argument.
    BadName,
}

impl std::fmt::Display for GmRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GmRefused::NotACommand => write!(f, "not a command line"),
            GmRefused::UnknownCommand(c) => write!(f, "{c} is not a command the bots send"),
            GmRefused::BadArguments(c) => write!(f, "bad arguments for {c}"),
            GmRefused::BadName => write!(f, "the bot's name is not usable as an argument"),
        }
    }
}

/// A number token: digits with an optional leading `-` and, if `frac`, one `.`.
fn is_number(tok: &str, frac: bool) -> bool {
    let body = tok.strip_prefix('-').unwrap_or(tok);
    let mut dots = 0;
    !body.is_empty()
        && body.len() <= 12
        && body != "."
        && body.chars().all(|c| match c {
            '0'..='9' => true,
            '.' if frac => {
                dots += 1;
                dots == 1
            }
            _ => false,
        })
}

fn is_uint(tok: &str) -> bool {
    !tok.starts_with('-') && is_number(tok, false)
}

/// Whether `name` can stand as one command argument: letters and digits (what the server allows
/// in a character name), nothing the command parser could split or reinterpret.
fn usable_name(name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= 24 && name.chars().all(char::is_alphanumeric)
}

/// Plans the GM line `line` for the bot called `own_name`: the short forms the goals write
/// (`/level 36`, `/labor 2000 [skill]`, `/move x y z`, `/item add self <id> <n> [grade]`,
/// `/gold add self <g> [s [c]]`) become lines that can only hit the bot itself. Anything else is
/// refused, `/build` included (see [`plan_build`]).
pub fn plan(line: &str, own_name: &str) -> Result<GmWire, GmRefused> {
    let line = line.trim();
    if !line.starts_with('/') || line.chars().any(char::is_control) {
        return Err(GmRefused::NotACommand);
    }
    let toks: Vec<&str> = line.split_whitespace().collect();
    let (cmd, args) = (toks[0], &toks[1..]);
    // `/<cmd> <own name> <args>`: the server finds the character by name first.
    let explicit = || -> Result<GmWire, GmRefused> {
        if !usable_name(own_name) {
            return Err(GmRefused::BadName);
        }
        Ok(GmWire {
            line: format!("{cmd} {own_name} {}", args.join(" ")),
            pre: Pre::ClearTarget,
        })
    };
    // `self` is in the line already.
    let as_is = || GmWire {
        line: toks.join(" "),
        pre: Pre::None,
    };
    let bad = |c: &'static str| Err(GmRefused::BadArguments(c));
    match cmd {
        "/level" => {
            if args.len() == 1 && is_uint(args[0]) {
                explicit()
            } else {
                bad("/level")
            }
        }
        "/labor" => {
            if (1..=2).contains(&args.len()) && args.iter().all(|a| is_number(a, false)) {
                explicit()
            } else {
                bad("/labor")
            }
        }
        "/move" => {
            if args.len() == 3 && args.iter().all(|a| is_number(a, true)) {
                explicit()
            } else {
                bad("/move")
            }
        }
        "/item" => {
            if (4..=5).contains(&args.len())
                && args[0] == "add"
                && args[1] == "self"
                && args[2..].iter().all(|a| is_uint(a))
            {
                Ok(as_is())
            } else {
                bad("/item")
            }
        }
        "/gold" => {
            if (3..=5).contains(&args.len())
                && args[0] == "add"
                && args[1] == "self"
                && args[2..].iter().all(|a| is_number(a, false))
            {
                Ok(as_is())
            } else {
                bad("/gold")
            }
        }
        other => Err(GmRefused::UnknownCommand(other.to_string())),
    }
}

/// The `/build <steps>` line. It advances the house the bot has *targeted*, and nothing else: the
/// caller targets its own house in the same flush ([`Pre::HouseTarget`]).
pub fn plan_build(steps: u32) -> GmWire {
    GmWire {
        line: format!("/build {}", steps.max(1)),
        pre: Pre::HouseTarget,
    }
}

/// Checks that `name` can stand unquoted as a MySQL database name in the statements: letters,
/// digits, `_` and `$`, not empty and at most 64 long.
pub fn check_db_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "'{name}' is not a usable database name (letters, digits, _ and $; at most 64)"
        ))
    }
}

/// Whether `name` can stand in a single-quoted SQL string as it is: the characters a login name
/// is made of (letters, digits, `_ . - @`), at most 32 of them.
fn check_account_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.chars().count() <= 32
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-' | '@'));
    if ok {
        Ok(())
    } else {
        Err(format!("'{name}' is not a usable account name"))
    }
}

/// The statements for exactly the accounts `accounts` (the roster's: `pb1`, `pb2`, ...) in the
/// databases `game_db` (accounts, characters) and `login_db` (users). Fails when a database or
/// account name is not usable, or when there is no account.
pub fn grant_sql(accounts: &[String], game_db: &str, login_db: &str) -> Result<String, String> {
    check_db_name(game_db)?;
    check_db_name(login_db)?;
    if accounts.is_empty() {
        return Err("the roster has no bots yet: start the bots once, then run this again".into());
    }
    for a in accounts {
        check_account_name(a)?;
    }
    let list = accounts
        .iter()
        .map(|a| format!("'{a}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let (first, last) = (&accounts[0], &accounts[accounts.len() - 1]);
    let n = accounts.len();
    Ok(format!(
        "-- Gives the {n} playerbots accounts ({first} ... {last}) GM access level {GM_LEVEL}.\n\
         -- The accounts are named one by one, not by pattern: no other account is touched.\n\
         -- Run it in the MySQL console of the AAEmu server after the bots have logged in once\n\
         -- (the server creates the account rows on the first login), then set\n\
         -- bots.gm_bootstrap = true in playerbots.toml.\n\
         UPDATE {game_db}.accounts SET access_level = {GM_LEVEL}\n\
         \x20WHERE account_id IN (SELECT id FROM {login_db}.users WHERE username IN ({list}));\n\
         UPDATE {game_db}.characters SET access_level = {GM_LEVEL}\n\
         \x20WHERE account_id IN (SELECT id FROM {login_db}.users WHERE username IN ({list}));\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "Aleron";

    fn names(ids: &[u32]) -> Vec<String> {
        ids.iter().map(|i| format!("pb{i}")).collect()
    }

    fn sql(accounts: &[String]) -> String {
        grant_sql(accounts, DEFAULT_GAME_DB, DEFAULT_LOGIN_DB).unwrap()
    }

    #[test]
    fn the_statements_name_both_databases_and_the_exact_accounts() {
        let s = sql(&names(&[1, 2, 7]));
        assert!(s.contains("UPDATE aaemu_game.accounts SET access_level = 100"));
        assert!(s.contains("UPDATE aaemu_game.characters SET access_level = 100"));
        assert_eq!(
            s.matches("aaemu_login.users WHERE username IN ('pb1', 'pb2', 'pb7')")
                .count(),
            2
        );
        assert!(!s.contains("REGEXP"), "no pattern: {s}");
    }

    #[test]
    fn the_output_is_the_expected_text() {
        assert_eq!(
            sql(&names(&[1, 2])),
            "-- Gives the 2 playerbots accounts (pb1 ... pb2) GM access level 100.\n\
             -- The accounts are named one by one, not by pattern: no other account is touched.\n\
             -- Run it in the MySQL console of the AAEmu server after the bots have logged in once\n\
             -- (the server creates the account rows on the first login), then set\n\
             -- bots.gm_bootstrap = true in playerbots.toml.\n\
             UPDATE aaemu_game.accounts SET access_level = 100\n \
             WHERE account_id IN (SELECT id FROM aaemu_login.users WHERE username IN ('pb1', 'pb2'));\n\
             UPDATE aaemu_game.characters SET access_level = 100\n \
             WHERE account_id IN (SELECT id FROM aaemu_login.users WHERE username IN ('pb1', 'pb2'));\n"
        );
    }

    #[test]
    fn other_database_names_are_used_in_every_statement() {
        let s = grant_sql(&names(&[1]), "lane_game", "lane_login").unwrap();
        assert!(s.contains("UPDATE lane_game.accounts SET access_level = 100"));
        assert!(s.contains("UPDATE lane_game.characters SET access_level = 100"));
        assert_eq!(s.matches("SELECT id FROM lane_login.users").count(), 2);
        assert!(!s.contains("aaemu_"));
    }

    #[test]
    fn unusable_database_and_account_names_are_refused() {
        let long = "x".repeat(65);
        for bad in [
            "",
            "a b",
            "x;DROP TABLE y",
            "a.b",
            "`a`",
            "\u{e9}",
            long.as_str(),
        ] {
            assert!(grant_sql(&names(&[1]), bad, "l").is_err(), "{bad:?}");
            assert!(grant_sql(&names(&[1]), "g", bad).is_err(), "{bad:?}");
        }
        assert!(check_db_name("lane_7$x").is_ok());
        // a quote or a backslash in an account name would end the SQL string
        for bad in ["pb1'", "pb1' OR '1'='1", "pb\\1", "a b", "", "pb1;"] {
            assert!(
                grant_sql(&[bad.to_string()], "g", "l").is_err(),
                "account {bad:?}"
            );
        }
        // no account at all is an error, not a statement that matches nothing (or everything)
        assert!(grant_sql(&[], "g", "l").is_err());
        // names with the characters a prefix may have are fine
        let ok = vec!["bot_.-@1".to_string(), "b\u{e9}t2".to_string()];
        assert!(grant_sql(&ok, "g", "l").is_ok());
    }

    // ---- who a command hits ------------------------------------------------------------

    fn planned(line: &str) -> GmWire {
        plan(line, ME).unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    #[test]
    fn target_or_self_commands_name_the_bot_and_clear_the_target() {
        let w = planned("/level 36");
        assert_eq!(
            (w.line.as_str(), w.pre),
            ("/level Aleron 36", Pre::ClearTarget)
        );
        let w = planned("/labor 2000");
        assert_eq!(
            (w.line.as_str(), w.pre),
            ("/labor Aleron 2000", Pre::ClearTarget)
        );
        let w = planned("/labor -50 8");
        assert_eq!(
            (w.line.as_str(), w.pre),
            ("/labor Aleron -50 8", Pre::ClearTarget)
        );
        let w = planned("/move 12345.5 -20.0 130.25");
        assert_eq!(
            (w.line.as_str(), w.pre),
            ("/move Aleron 12345.5 -20.0 130.25", Pre::ClearTarget)
        );
    }

    #[test]
    fn the_explicit_self_forms_pass_unchanged() {
        let w = planned("/item add self 8000 12");
        assert_eq!(
            (w.line.as_str(), w.pre),
            ("/item add self 8000 12", Pre::None)
        );
        let w = planned("/item add self 8000 12 3");
        assert_eq!(w.line, "/item add self 8000 12 3");
        let w = planned("/gold add self 1");
        assert_eq!((w.line.as_str(), w.pre), ("/gold add self 1", Pre::None));
        let w = planned("/gold add self 1 20 30");
        assert_eq!(w.line, "/gold add self 1 20 30");
        // spacing is normalised
        assert_eq!(
            planned("  /item   add self  5  6 ").line,
            "/item add self 5 6"
        );
    }

    #[test]
    fn every_planned_line_hits_only_the_bot() {
        for line in [
            "/level 36",
            "/labor 2000",
            "/labor 94 12",
            "/move 1.0 2.0 3.0",
            "/item add self 1 1",
            "/gold add self 5",
        ] {
            let w = planned(line);
            assert!(w.is_self_only(ME), "{line} -> {w:?}");
        }
        let b = plan_build(4);
        assert_eq!((b.line.as_str(), b.pre), ("/build 4", Pre::HouseTarget));
        assert!(b.is_self_only(ME));
        assert_eq!(plan_build(0).line, "/build 1");
        // lines that name somebody else, or rely on the target, are not self-only
        for (line, pre) in [
            ("/level Bob 36", Pre::ClearTarget),
            ("/level 36", Pre::None),
            ("/move 1 2 3", Pre::ClearTarget),
            ("/gold add Bob 5", Pre::None),
            ("/gold add target 5", Pre::None),
            ("/item add target 1 1", Pre::None),
            ("/labor 5", Pre::None),
        ] {
            let w = GmWire {
                line: line.into(),
                pre,
            };
            assert!(!w.is_self_only(ME), "{line} {pre:?}");
        }
    }

    #[test]
    fn other_commands_and_shapes_are_refused() {
        for line in [
            "/shutdown",
            "/kick Bob",
            "/kill",
            "/teleport Bob",
            "/revive",
            "/item add Bob 1 1",
            "/item add target 1 1",
            "/item remove self 1 1",
            "/item add self x 1",
            "/item add self 1 -1",
            "/gold add Bob 1",
            "/gold add target 1",
            "/gold remove self 1",
            "/gold add self 1 2 3 4",
            "/level Bob 36",
            "/level 36 37",
            "/level -3",
            "/level",
            "/labor",
            "/labor 1 2 3",
            "/move tome",
            "/move Bob 1 2 3",
            "/move 1 2",
            "/move 1 2 x",
            "/move NaN 1 2",
            "/move inf 1 2",
            "/build 3",
            "level 36",
            "",
            "/",
        ] {
            assert!(plan(line, ME).is_err(), "{line:?} should be refused");
        }
        // control characters hide a second command or a leading slash
        for line in [
            "/level 5\n/gold add Bob 99",
            "/level 5\0",
            "\0/level 5",
            "/item add self 1 1\r\n/kick",
        ] {
            assert_eq!(plan(line, ME), Err(GmRefused::NotACommand), "{line:?}");
        }
        assert_eq!(
            plan("/kill", ME),
            Err(GmRefused::UnknownCommand("/kill".into()))
        );
    }

    #[test]
    fn an_unusable_own_name_refuses_the_target_commands_only() {
        for name in ["", "Two Words", "A/B", "Bob;", "\0", &"x".repeat(30)] {
            assert_eq!(plan("/level 5", name), Err(GmRefused::BadName), "{name:?}");
            assert_eq!(plan("/move 1 2 3", name), Err(GmRefused::BadName));
            assert_eq!(plan("/labor 5", name), Err(GmRefused::BadName));
        }
        // `self` needs no name
        assert!(plan("/item add self 1 1", "").is_ok());
        // a non-ASCII character name is a fine argument
        assert!(plan("/level 5", "\u{410}\u{43b}\u{435}\u{43a}").is_ok());
    }
}
