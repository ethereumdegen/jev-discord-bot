//! The rules, with no I/O: whether a message is worth a judgment, what the
//! verdict means, and which ladder step a strike lands on.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A message as the gateway delivered it, stripped to what the rules need.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Incoming {
    pub guild_id: String,
    pub message_id: String,
    pub channel_id: String,
    pub author_id: String,
    pub username: String,
    pub content: String,
    pub author_is_bot: bool,
    /// When they joined the server, if the gateway said.
    pub joined_at: Option<DateTime<Utc>>,
    pub role_ids: Vec<String>,
    pub mentions_everyone: bool,
}

/// One rung of a server's ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Log the strike, do nothing else.
    None,
    /// Delete the message and post a short notice.
    Warn,
    /// Delete, and time the author out.
    Timeout,
    /// Delete and kick.
    Kick,
    /// Delete and ban (the ban also clears their last hour of messages).
    Ban,
}

impl Step {
    pub fn as_str(self) -> &'static str {
        match self {
            Step::None => "none",
            Step::Warn => "warn",
            Step::Timeout => "timeout",
            Step::Kick => "kick",
            Step::Ban => "ban",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Step::None),
            "warn" => Some(Step::Warn),
            "timeout" => Some(Step::Timeout),
            "kick" => Some(Step::Kick),
            "ban" => Some(Step::Ban),
            _ => None,
        }
    }

    pub fn deletes(self) -> bool {
        !matches!(self, Step::None)
    }

    /// What people are told comes next, in the warning.
    pub fn describe(self) -> &'static str {
        match self {
            Step::None => "nothing",
            Step::Warn => "another warning",
            Step::Timeout => "a time-out",
            Step::Kick => "a kick",
            Step::Ban => "a ban",
        }
    }
}

/// The step for strike `n` (1-based); past the end of the ladder, the last step repeats.
pub fn step_for(ladder: &[Step], n: usize) -> Step {
    if ladder.is_empty() {
        return Step::Warn;
    }
    ladder[n.clamp(1, ladder.len()) - 1]
}

pub fn parse_ladder(values: &[String]) -> Option<Vec<Step>> {
    if values.is_empty() || values.len() > 10 {
        return None;
    }
    values.iter().map(|v| Step::parse(v)).collect()
}

/// Account creation time from a Discord snowflake (ms since 2015, shifted 22 bits).
pub fn created_at(snowflake: &str) -> Option<DateTime<Utc>> {
    let id: u64 = snowflake.parse().ok()?;
    DateTime::from_timestamp_millis(((id >> 22) + 1_420_070_400_000) as i64)
}

pub fn has_link(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["http://", "https://", "www.", "discord.gg/", "discord.com/invite", "t.me/", "bit.ly/"].iter().any(|m| lower.contains(m))
}

/// Every message with text is judged, whoever sent it: only bots and
/// text-less messages (an image on its own) are skipped.
pub fn needs_judging(incoming: &Incoming) -> bool {
    !incoming.author_is_bot && !incoming.content.trim().is_empty()
}

/// What Jev said about a message.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub kind: String,
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: f64,
    /// Probability it tries to get people to click, DM or pay.
    pub lure: f64,
}

impl Verdict {
    fn p(&self, kind: &str) -> f64 {
        self.probabilities.get(kind).copied().unwrap_or(0.0)
    }

    /// How likely it breaks the spam rule.
    pub fn bad(&self) -> f64 {
        (self.p("spam") + self.p("scam_or_phishing") + self.p("self_promo_off_topic")).min(1.0)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// At or above: a confirmed offense.
    pub confident: f64,
    /// At or above (below confident): listed for review, no strike.
    pub flag: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Fine,
    Review,
    Offense,
}

pub fn decide(verdict: &Verdict, thresholds: Thresholds) -> Decision {
    let bad = verdict.bad();
    if bad >= thresholds.confident {
        Decision::Offense
    } else if bad >= thresholds.flag {
        Decision::Review
    } else {
        Decision::Fine
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn verdict(spam: f64, scam: f64, promo: f64) -> Verdict {
        let probabilities = [("legit", 1.0 - spam - scam - promo), ("spam", spam), ("scam_or_phishing", scam), ("self_promo_off_topic", promo)]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect();
        Verdict { kind: "x".into(), probabilities, confidence: 0.9, lure: 0.0 }
    }

    #[test]
    fn the_ladder() {
        let ladder = [Step::Warn, Step::Kick, Step::Ban];
        assert_eq!(step_for(&ladder, 1), Step::Warn);
        assert_eq!(step_for(&ladder, 2), Step::Kick);
        assert_eq!(step_for(&ladder, 3), Step::Ban);
        assert_eq!(step_for(&ladder, 9), Step::Ban, "the last step repeats");
        assert_eq!(step_for(&[], 1), Step::Warn);
        assert_eq!(parse_ladder(&["warn".into(), "timeout".into()]), Some(vec![Step::Warn, Step::Timeout]));
        assert_eq!(parse_ladder(&["warn".into(), "nuke".into()]), None);
        assert_eq!(parse_ladder(&[]), None);
    }

    #[test]
    fn decisions() {
        let t = Thresholds { confident: 0.9, flag: 0.6 };
        assert_eq!(decide(&verdict(0.95, 0.0, 0.0), t), Decision::Offense);
        assert_eq!(decide(&verdict(0.0, 0.5, 0.45), t), Decision::Offense, "the kinds add up");
        assert_eq!(decide(&verdict(0.5, 0.0, 0.2), t), Decision::Review);
        assert_eq!(decide(&verdict(0.1, 0.1, 0.1), t), Decision::Fine);
    }

    #[test]
    fn every_message_with_text_is_judged() {
        let now = Utc::now();
        let mut m = Incoming {
            guild_id: "g".into(),
            message_id: "1".into(),
            channel_id: "2".into(),
            author_id: "175928847299117063".into(),
            username: "m".into(),
            content: "anyone tried the new agent SDK?".into(),
            author_is_bot: false,
            joined_at: Some(now - Duration::days(60)),
            role_ids: vec![],
            mentions_everyone: false,
        };
        assert!(needs_judging(&m), "a regular's plain chat");
        m.content = "  ".into();
        assert!(!needs_judging(&m), "no text");
        m.content = "hi".into();
        m.author_is_bot = true;
        assert!(!needs_judging(&m), "a bot");
    }

    #[test]
    fn snowflakes_and_links() {
        assert_eq!(created_at("175928847299117063").unwrap().format("%Y-%m-%d").to_string(), "2016-04-30");
        assert!(has_link("free nitro at discord.gg/abc"));
        assert!(!has_link("I use https less than I should"));
    }
}
