//! Social behavior: reacting to chat (stub).
//!
//! Not a goal the brain chooses but a hook that runs on every brain tick, whatever the bot is
//! doing. The stub only empties the chat inbox (so [`BotState::inbox`](crate::bot::state::BotState)
//! stays small).
//!
//! To implement: look at `ctx.state.inbox` (whispers `kind == -3`, local chat `kind == 0` from
//! characters near the bot), decide whether to answer (rate limit per bot, never answer a bot of
//! our own: `ctx.hub.is_bot_char`), and say the reply with `ctx.say`. With the LLM add-on on, ask
//! `pb_llm::Llm` for the reply on a spawned task and hand the text back over a channel; never
//! block the tick on the network.

use crate::bot::ctx::Ctx;

/// Handles the chat the bot heard since the last tick.
pub fn tick(ctx: &mut Ctx<'_>) {
    if !ctx.state.inbox.is_empty() {
        ctx.state.inbox.clear();
    }
}
