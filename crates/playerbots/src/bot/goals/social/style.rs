//! How one bot talks: its personality ([`Profile`]) turned into the numbers the chat decisions
//! scale by. Pure arithmetic, no clock and no randomness.
//!
//! * **Chattiness** (`chatty`) scales the chance to answer an overheard line, the pauses between
//!   lines, the chance to remark on what happened, the gap between two ambient lines, whether and
//!   how often the bot speaks on the public channels, and the per-bot cap on unasked lines.
//!   0.5 is the plain chat from before personalities.
//! * **Humor** (`joker`) is the chance that an ambient line is a joke and that small talk gets a
//!   quip.
//! * **Coarseness** (`crude`) decides the bot's own profanity level (clean below 0.4, mild below
//!   0.75, strong above) and how often it uses it; the operator's `[personality] profanity` caps
//!   the level.
//!
//! The process-wide limits (`[social]`, `[personality]`) stay authoritative: a chatterbox only
//! takes a larger share of them.

use pb_llm::persona::{ChatStyle, Profanity};

use super::lines::Tongue;
use crate::config::PersonalityCfg;
use crate::roster::Profile;

/// Mean gap between two ambient lines of a chatterbox (chattiness 1), in milliseconds; a bot of
/// chattiness `c` waits `AMBIENT_BASE_MS * 20^(1 - c)` (13 minutes at 0.5, 50 at 0.05).
pub const AMBIENT_BASE_MS: u64 = 3 * 60_000;
/// Mean gap between two channel lines of a chatterbox, in milliseconds; `CHANNEL_BASE_MS *
/// 30^(1 - c)` (about 2 hours at 0.5).
pub const CHANNEL_BASE_MS: u64 = 20 * 60_000;
/// Bots below this chattiness never speak on the public channels.
pub const CHANNEL_MIN_CHATTY: f32 = 0.3;
/// The most unasked lines a chatterbox says within [`UNASKED_SPAN_MS`].
pub const UNASKED_MAX: usize = 5;
/// The window of the per-bot cap on unasked lines.
pub const UNASKED_SPAN_MS: u64 = 10 * 60_000;

/// The chat style of one bot. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// Speaks unasked, 0..=1.
    pub chatty: f32,
    /// Coarse language, 0..=1.
    pub crude: f32,
    /// Jokes and banter, 0..=1.
    pub joker: f32,
    /// The operator's cap.
    pub cap: Profanity,
}

impl Default for Style {
    fn default() -> Self {
        Style::NEUTRAL
    }
}

fn unit(x: f32) -> f32 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.5
    }
}

fn scale((lo, hi): (u64, u64), f: f32) -> (u64, u64) {
    ((lo as f32 * f) as u64, (hi as f32 * f) as u64)
}

impl Style {
    /// Every trait at 0.5 under the default cap: the plain chat from before personalities, with
    /// a little mild swearing.
    pub const NEUTRAL: Style = Style {
        chatty: 0.5,
        crude: 0.5,
        joker: 0.5,
        cap: Profanity::Mild,
    };

    /// The style of a bot with `profile`. With the personalities switched off every bot talks
    /// like the neutral profile and stays clean.
    pub fn new(profile: &Profile, cfg: &PersonalityCfg) -> Style {
        if !cfg.enabled {
            return Style {
                crude: 0.0,
                cap: Profanity::Off,
                ..Style::NEUTRAL
            };
        }
        Style {
            chatty: unit(profile.chatty),
            crude: unit(profile.crude),
            joker: unit(profile.joker),
            cap: cfg.profanity,
        }
    }

    /// The bot's own profanity level, before the cap.
    pub fn own_level(&self) -> Profanity {
        if self.crude >= 0.75 {
            Profanity::Strong
        } else if self.crude >= 0.4 {
            Profanity::Mild
        } else {
            Profanity::Off
        }
    }

    /// The coarsest language the bot uses: its own level under the operator's cap. Every line it
    /// says is filtered at this level.
    pub fn level(&self) -> Profanity {
        self.own_level().min(self.cap)
    }

    /// How the bot swears, for the line tables.
    pub fn tongue(&self) -> Tongue {
        Tongue {
            level: self.level(),
            crude: self.crude,
        }
    }

    /// The chance to answer an overheard line that does not name the bot (`base` is
    /// `social.reply_chance`): a third of it for a quiet bot, 1.75 times for a chatterbox.
    pub fn reply_chance(&self, base: f32) -> f32 {
        (base * (0.25 + 1.5 * self.chatty)).clamp(0.0, 1.0)
    }

    /// The pause between two replies: a chatterbox answers sooner (0.75 times), a quiet bot later
    /// (1.25 times).
    pub fn reply_gap(&self, gap: (u64, u64)) -> (u64, u64) {
        scale(gap, 1.25 - 0.5 * self.chatty)
    }

    /// The least time between two unasked lines, in milliseconds (`base_secs` is
    /// `social.chatter_gap_secs`): half of it for a chatterbox, twice for a quiet bot.
    pub fn chatter_gap_ms(&self, base_secs: u64) -> u64 {
        let f = 2f32.powf(1.0 - 2.0 * self.chatty);
        (base_secs.saturating_mul(1000) as f32 * f) as u64
    }

    /// The chance to remark on something that happened, of a base chance per kind of event.
    pub fn remark_chance(&self, base: f32) -> f32 {
        (base * (0.4 + 1.2 * self.chatty)).clamp(0.0, 1.0)
    }

    /// The chance to greet a bot standing close by, at a look around.
    pub fn hello_chance(&self) -> f32 {
        self.remark_chance(0.4)
    }

    /// The mean gap between two ambient lines, in milliseconds.
    pub fn ambient_gap_ms(&self) -> u64 {
        (AMBIENT_BASE_MS as f32 * 20f32.powf(1.0 - self.chatty)) as u64
    }

    /// The mean gap between two lines on the public channels, in milliseconds; `None` for a bot
    /// too quiet to use them.
    pub fn channel_gap_ms(&self) -> Option<u64> {
        (self.chatty >= CHANNEL_MIN_CHATTY)
            .then(|| (CHANNEL_BASE_MS as f32 * 30f32.powf(1.0 - self.chatty)) as u64)
    }

    /// The most unasked lines within [`UNASKED_SPAN_MS`]: one for a quiet bot, three for the
    /// neutral one, [`UNASKED_MAX`] for a chatterbox.
    pub fn unasked_limit(&self) -> usize {
        (1 + (4.0 * self.chatty).round() as usize).min(UNASKED_MAX)
    }

    /// The chance that an ambient line is a joke.
    pub fn joke_chance(&self) -> f32 {
        0.1 + 0.8 * self.joker
    }

    /// The chance that small talk gets a quip instead of a plain answer.
    pub fn quip_chance(&self) -> f32 {
        0.35 * self.joker
    }

    /// The chance that small talk gets a coarse answer (none for a clean bot).
    pub fn crude_reply_chance(&self) -> f32 {
        if self.level() == Profanity::Off {
            0.0
        } else {
            0.3 * self.crude
        }
    }

    /// The style for the LLM persona.
    pub fn llm(&self) -> ChatStyle {
        let pct = |x: f32| (unit(x) * 100.0).round() as u8;
        ChatStyle {
            chatty: pct(self.chatty),
            joker: pct(self.joker),
            profanity: self.level(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(chatty: f32, crude: f32, joker: f32) -> Style {
        Style {
            chatty,
            crude,
            joker,
            cap: Profanity::Strong,
        }
    }

    #[test]
    fn the_neutral_style_keeps_the_plain_chat() {
        let n = Style::NEUTRAL;
        assert!((n.reply_chance(0.4) - 0.4).abs() < 1e-6);
        assert_eq!(n.reply_gap((8_000, 14_000)), (8_000, 14_000));
        assert_eq!(n.chatter_gap_ms(60), 60_000);
        assert!((n.remark_chance(0.7) - 0.7).abs() < 1e-6);
        assert!((n.hello_chance() - 0.4).abs() < 1e-6);
        assert_eq!(n.unasked_limit(), 3);
        assert_eq!(n.level(), Profanity::Mild);
    }

    #[test]
    fn a_chatterbox_talks_more_and_a_quiet_bot_less() {
        let loud = style(1.0, 0.0, 0.5);
        let quiet = style(0.05, 0.0, 0.5);
        assert!(loud.reply_chance(0.4) > 0.65 && quiet.reply_chance(0.4) < 0.15);
        assert!(loud.reply_gap((8_000, 14_000)).0 < 8_000);
        assert!(quiet.reply_gap((8_000, 14_000)).0 > 8_000);
        assert_eq!(loud.chatter_gap_ms(60), 30_000);
        assert!(quiet.chatter_gap_ms(60) > 110_000);
        assert_eq!(loud.ambient_gap_ms(), AMBIENT_BASE_MS);
        assert!(quiet.ambient_gap_ms() > 45 * 60_000);
        assert!((12 * 60_000..15 * 60_000).contains(&Style::NEUTRAL.ambient_gap_ms()));
        assert_eq!(loud.channel_gap_ms(), Some(CHANNEL_BASE_MS));
        assert_eq!(quiet.channel_gap_ms(), None);
        assert_eq!((loud.unasked_limit(), quiet.unasked_limit()), (5, 1));
        assert!(loud.remark_chance(0.9) == 1.0 && quiet.remark_chance(0.9) < 0.45);
        // nonsense from a hand-edited roster is clamped
        let odd = Style::new(
            &Profile {
                chatty: f32::NAN,
                crude: 7.0,
                ..Profile::NEUTRAL
            },
            &PersonalityCfg::default(),
        );
        assert_eq!(odd.chatty, 0.5);
        assert_eq!(odd.crude, 1.0);
    }

    #[test]
    fn the_operator_caps_the_language() {
        let sailor = Profile {
            crude: 0.9,
            ..Profile::NEUTRAL
        };
        let mut cfg = PersonalityCfg::default();
        assert_eq!(Style::new(&sailor, &cfg).own_level(), Profanity::Strong);
        assert_eq!(Style::new(&sailor, &cfg).level(), Profanity::Mild);
        cfg.profanity = Profanity::Strong;
        assert_eq!(Style::new(&sailor, &cfg).level(), Profanity::Strong);
        cfg.profanity = Profanity::Off;
        assert_eq!(Style::new(&sailor, &cfg).level(), Profanity::Off);
        assert_eq!(Style::new(&sailor, &cfg).crude_reply_chance(), 0.0);
        assert_eq!(Style::new(&sailor, &cfg).llm().profanity, Profanity::Off);
        // a polite bot stays polite under any cap
        let polite = Profile {
            crude: 0.1,
            ..Profile::NEUTRAL
        };
        cfg.profanity = Profanity::Strong;
        assert_eq!(Style::new(&polite, &cfg).level(), Profanity::Off);
        // personalities off: neutral and clean
        cfg.enabled = false;
        let s = Style::new(&sailor, &cfg);
        assert_eq!((s.chatty, s.level()), (0.5, Profanity::Off));
    }

    #[test]
    fn the_llm_gets_the_style_in_percent() {
        let s = style(0.9, 0.5, 0.2).llm();
        assert_eq!((s.chatty, s.joker, s.profanity), (90, 20, Profanity::Mild));
    }
}
