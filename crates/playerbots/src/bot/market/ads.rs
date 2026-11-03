//! Trade-chat sales: advertisements, buyers and trades with players (owned by the market
//! feature, goal `market`).
//!
//! [`AdState`] is the bot's memory of what it advertised and who answered. It lives in
//! [`Inner`](super::Inner) (`state.market.get_mut()`); until the feature fills it in, it is empty
//! and everything here does nothing. Already routed here:
//!
//! * [`AdState::on_chat`]: every shout (1), trade (2) and group-finding (3) line and every whisper
//!   the bot hears, from the social duty (`goals::social`), besides the duty's own handling. It
//!   is only called once the bot has market memory (see
//!   [`MarketState::inner_mut`](super::MarketState::inner_mut)).
//! * `ctx.cfg.market` (`chat_ads`, `sell_to_players`, `meet_at`, `max_ads_per_hour`,
//!   `ad_interval_secs`) and [`Config::may_sell_to_players`](crate::config::Config::may_sell_to_players),
//!   the rule for selling to real players.
//! * [`Ledger::sellable_surplus`](super::ledger::Ledger::sellable_surplus): the goods the bot may
//!   offer.
//! * `cs::chat_type::{SHOUT, TRADE, GROUP_FIND, REGION, ALLY}` for the lines it sends; every
//!   chat builder refuses a line that starts with `/`.

use aa_proto::cs::chat_type;

/// The chat types the market hears: shout, trade, group finding and whispers (the server's
/// `ChatType`, [`aa_proto::cs::chat_type`]).
pub const HEARD_KINDS: [i16; 4] = [
    chat_type::SHOUT,
    chat_type::TRADE,
    chat_type::GROUP_FIND,
    chat_type::WHISPER,
];

/// Whether the social duty hands lines of chat type `kind` to [`AdState::on_chat`].
pub fn hears(kind: i16) -> bool {
    HEARD_KINDS.contains(&kind)
}

/// What the bot remembers of its advertisements and buyers. Empty until the feature exists.
#[derive(Debug, Default)]
pub struct AdState {}

impl AdState {
    /// The bot heard a chat line. `kind` is the server's chat type (1 shout, 2 trade, 3 group
    /// finding, -3 whisper), `sender` the speaker's name, `text` the line (at most 200
    /// characters) and `now_ms` the bot clock.
    pub fn on_chat(&mut self, _kind: i16, _sender: &str, _text: &str, _now_ms: u64) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_state_ignores_chat() {
        let mut ads = AdState::default();
        ads.on_chat(2, "Bob", "WTB copper ore", 10);
        assert_eq!(std::mem::size_of::<AdState>(), 0);
    }
}
