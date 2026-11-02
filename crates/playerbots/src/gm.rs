//! `playerbots grant-gm-sql`: the SQL that gives the bot accounts GM access.
//!
//! Bots only use GM chat commands (`/move`, `/item add`, ...) when `bots.gm_bootstrap` is on, and
//! the server only runs them for access level 100. The login server creates the accounts (and the
//! game server their `accounts` rows) on the first login, so run the statements after the bots
//! have logged in once.

/// Access level the server's GM commands need.
pub const GM_LEVEL: u32 = 100;

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

/// The statements for the accounts named `<account_prefix><number>`.
pub fn grant_sql(account_prefix: &str) -> String {
    let re = regex_escape(account_prefix);
    format!(
        "-- Gives the playerbots accounts ({account_prefix}1, {account_prefix}2, ...) GM access level {GM_LEVEL}.\n\
         -- Run it in the MySQL console of the AAEmu server after the bots have logged in once\n\
         -- (the server creates the account rows on the first login), then set\n\
         -- bots.gm_bootstrap = true in playerbots.toml.\n\
         UPDATE aaemu_game.accounts SET access_level = {GM_LEVEL}\n\
         \x20WHERE account_id IN (SELECT id FROM aaemu_login.users WHERE username REGEXP '^{re}[0-9]+$');\n\
         UPDATE aaemu_game.characters SET access_level = {GM_LEVEL}\n\
         \x20WHERE account_id IN (SELECT id FROM aaemu_login.users WHERE username REGEXP '^{re}[0-9]+$');\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_statements_name_both_databases_and_the_prefix() {
        let sql = grant_sql("pb");
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
        assert!(grant_sql("x.y").contains("'^x\\\\.y[0-9]+$'"));
    }
}
