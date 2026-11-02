//! `playerbots grant-gm-sql`: the SQL that gives the bot accounts GM access.
//!
//! Bots only use GM chat commands (`/move`, `/item add`, ...) when `bots.gm_bootstrap` is on, and
//! the server only runs them for access level 100. The login server creates the accounts (and the
//! game server their `accounts` rows) on the first login, so run the statements after the bots
//! have logged in once.
//!
//! The statements name the two MySQL databases of the server (`aaemu_game` and `aaemu_login` by
//! default; an isolated test server uses its own).

/// Access level the server's GM commands need.
pub const GM_LEVEL: u32 = 100;
/// The game server's database unless the server is set up differently.
pub const DEFAULT_GAME_DB: &str = "aaemu_game";
/// The login server's database unless the server is set up differently.
pub const DEFAULT_LOGIN_DB: &str = "aaemu_login";

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

/// Escapes `prefix` for a MySQL regular expression inside a single-quoted SQL string.
fn regex_escape(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 4);
    for c in prefix.chars() {
        if matches!(
            c,
            '\\' | '^' | '$' | '.' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}'
        ) {
            out.push_str("\\\\");
        }
        out.push(c);
    }
    out
}

/// The statements for the accounts named `<account_prefix><number>` in the databases `game_db`
/// (accounts, characters) and `login_db` (users). Fails when a database name is not usable.
pub fn grant_sql(account_prefix: &str, game_db: &str, login_db: &str) -> Result<String, String> {
    check_db_name(game_db)?;
    check_db_name(login_db)?;
    let re = regex_escape(account_prefix);
    Ok(format!(
        "-- Gives the playerbots accounts ({account_prefix}1, {account_prefix}2, ...) GM access level {GM_LEVEL}.\n\
         -- Run it in the MySQL console of the AAEmu server after the bots have logged in once\n\
         -- (the server creates the account rows on the first login), then set\n\
         -- bots.gm_bootstrap = true in playerbots.toml.\n\
         UPDATE {game_db}.accounts SET access_level = {GM_LEVEL}\n\
         \x20WHERE account_id IN (SELECT id FROM {login_db}.users WHERE username REGEXP '^{re}[0-9]+$');\n\
         UPDATE {game_db}.characters SET access_level = {GM_LEVEL}\n\
         \x20WHERE account_id IN (SELECT id FROM {login_db}.users WHERE username REGEXP '^{re}[0-9]+$');\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_sql(prefix: &str) -> String {
        grant_sql(prefix, DEFAULT_GAME_DB, DEFAULT_LOGIN_DB).unwrap()
    }

    #[test]
    fn the_statements_name_both_databases_and_the_prefix() {
        let sql = default_sql("pb");
        assert!(sql.contains("UPDATE aaemu_game.accounts SET access_level = 100"));
        assert!(sql.contains("UPDATE aaemu_game.characters SET access_level = 100"));
        assert_eq!(
            sql.matches("aaemu_login.users WHERE username REGEXP '^pb[0-9]+$'")
                .count(),
            2
        );
    }

    #[test]
    fn regex_characters_in_the_prefix_are_escaped() {
        assert_eq!(regex_escape("a.b"), "a\\\\.b");
        assert_eq!(regex_escape("bot_-@"), "bot_-@");
        assert!(default_sql("x.y").contains("'^x\\\\.y[0-9]+$'"));
    }

    #[test]
    fn the_default_output_is_the_original_text() {
        assert_eq!(
            default_sql("pb"),
            "-- Gives the playerbots accounts (pb1, pb2, ...) GM access level 100.\n\
             -- Run it in the MySQL console of the AAEmu server after the bots have logged in once\n\
             -- (the server creates the account rows on the first login), then set\n\
             -- bots.gm_bootstrap = true in playerbots.toml.\n\
             UPDATE aaemu_game.accounts SET access_level = 100\n \
             WHERE account_id IN (SELECT id FROM aaemu_login.users WHERE username REGEXP '^pb[0-9]+$');\n\
             UPDATE aaemu_game.characters SET access_level = 100\n \
             WHERE account_id IN (SELECT id FROM aaemu_login.users WHERE username REGEXP '^pb[0-9]+$');\n"
        );
    }

    #[test]
    fn other_database_names_are_used_in_every_statement() {
        let sql = grant_sql("pb", "lane_game", "lane_login").unwrap();
        assert!(sql.contains("UPDATE lane_game.accounts SET access_level = 100"));
        assert!(sql.contains("UPDATE lane_game.characters SET access_level = 100"));
        assert_eq!(sql.matches("SELECT id FROM lane_login.users").count(), 2);
        assert!(!sql.contains("aaemu_"));
    }

    #[test]
    fn unusable_database_names_are_refused() {
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
            assert!(grant_sql("pb", bad, "l").is_err(), "{bad:?}");
            assert!(grant_sql("pb", "g", bad).is_err(), "{bad:?}");
        }
        assert!(check_db_name("lane_7$x").is_ok());
    }
}
