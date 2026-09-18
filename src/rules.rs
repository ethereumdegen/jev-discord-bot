//! The rules, with no I/O: who a sender counts as, whether a message is worth
//! a judgment, and what to do with the verdict.
//!
//! Members are never kicked or banned automatically: a wrong call would throw
//! out someone who pays. The worst they get is a deleted message and an hour's
//! time-out, and the mods see it in #mod-log.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};

/// Visitors this new to the server are treated as likely drive-by spammers.
pub const NEW_DAYS: i64 = 7;
pub const MEMBER_TIMEOUT_HOURS: i64 = 1;

/// A message as the gateway delivered it, stripped to what the rules need.
#[derive(Debug, Clone)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    /// Not a member, in the server under a week.
    NewVisitor,
    Visitor,
    Member,
}

impl Sender {
    pub fn as_str(self) -> &'static str {
        match self {
            Sender::NewVisitor => "new_visitor",
            Sender::Visitor => "visitor",
            Sender::Member => "member",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Sender::Member => "a paying member",
            Sender::Visitor => "a visitor (not a paying member)",
            Sender::NewVisitor => "a visitor who joined the server this week",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// Report to the mods; the message stays.
    Flag,
    Delete,
    /// Delete, and an hour's time-out.
    Timeout,
    /// Delete and kick.
    Kick,
    /// Delete and ban (the ban also clears their last hour of messages).
    Ban,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::None => "none",
            Action::Flag => "flag",
            Action::Delete => "delete",
            Action::Timeout => "timeout",
            Action::Kick => "kick",
            Action::Ban => "ban",
        }
    }

    /// Whether it counts as a strike against the author.
    pub fn strikes(self) -> bool {
        matches!(self, Action::Timeout | Action::Kick | Action::Ban)
    }

    pub fn deletes(self) -> bool {
        matches!(self, Action::Delete | Action::Timeout | Action::Kick | Action::Ban)
    }
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

pub fn sender(incoming: &Incoming, member_roles: &[String], now: DateTime<Utc>) -> Sender {
    if incoming.role_ids.iter().any(|r| member_roles.contains(r)) {
        return Sender::Member;
    }
    match incoming.joined_at {
        Some(joined) if now - joined >= Duration::days(NEW_DAYS) => Sender::Visitor,
        _ => Sender::NewVisitor,
    }
}

pub fn is_staff(incoming: &Incoming, staff_roles: &[String]) -> bool {
    incoming.role_ids.iter().any(|r| staff_roles.contains(r))
}

/// The cheap check before paying for a judgment: established members' ordinary
/// messages are left alone; everything else, and anything with a link or an
/// @everyone, is judged.
pub fn needs_judging(incoming: &Incoming, sender: Sender, now: DateTime<Utc>) -> bool {
    if incoming.content.trim().is_empty() {
        return false;
    }
    let settled = incoming.joined_at.is_some_and(|joined| now - joined >= Duration::days(NEW_DAYS));
    let risky = has_link(&incoming.content) || incoming.mentions_everyone || incoming.content.contains("@everyone") || incoming.content.contains("@here");
    !(sender == Sender::Member && settled && !risky)
}

/// What Jev said.
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

    /// How likely it's something that shouldn't be here.
    pub fn bad(&self) -> f64 {
        (self.p("spam") + self.p("scam_or_phishing") + self.p("self_promo_off_topic")).min(1.0)
    }

    pub fn scam(&self) -> bool {
        self.kind == "scam_or_phishing" || (self.lure >= 0.8 && self.p("scam_or_phishing") >= 0.3)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// At or above: act.
    pub confident: f64,
    /// At or above (and below confident): report to the mods.
    pub flag: f64,
}

/// | Sender | Confident | Unsure |
/// |---|---|---|
/// | new visitor | scam: ban; spam: kick | delete |
/// | visitor | kick; ban on the 2nd strike or a scam | flag |
/// | member | delete + 1h time-out | flag |
pub fn decide(sender: Sender, verdict: &Verdict, strikes: i32, thresholds: Thresholds) -> Action {
    let bad = verdict.bad();
    if bad < thresholds.flag {
        return Action::None;
    }
    let confident = bad >= thresholds.confident;
    match (sender, confident) {
        (Sender::NewVisitor, true) if verdict.scam() => Action::Ban,
        (Sender::NewVisitor, true) => Action::Kick,
        (Sender::NewVisitor, false) => Action::Delete,
        (Sender::Visitor, true) if strikes >= 1 || verdict.scam() => Action::Ban,
        (Sender::Visitor, true) => Action::Kick,
        (Sender::Member, true) => Action::Timeout,
        (_, false) => Action::Flag,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(kind: &str, spam: f64, scam: f64, promo: f64, lure: f64) -> Verdict {
        let probabilities = [("legit", 1.0 - spam - scam - promo), ("spam", spam), ("scam_or_phishing", scam), ("self_promo_off_topic", promo)]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v))
            .collect();
        Verdict { kind: kind.into(), probabilities, confidence: 0.9, lure }
    }

    const T: Thresholds = Thresholds { confident: 0.9, flag: 0.6 };

    #[test]
    fn the_rules_table() {
        let scam = verdict("scam_or_phishing", 0.05, 0.93, 0.0, 0.95);
        let spam = verdict("spam", 0.95, 0.0, 0.0, 0.2);
        let unsure = verdict("spam", 0.5, 0.0, 0.2, 0.2);
        let fine = verdict("legit", 0.1, 0.05, 0.1, 0.1);
        assert_eq!(decide(Sender::NewVisitor, &scam, 0, T), Action::Ban);
        assert_eq!(decide(Sender::NewVisitor, &spam, 0, T), Action::Kick);
        assert_eq!(decide(Sender::NewVisitor, &unsure, 0, T), Action::Delete);
        assert_eq!(decide(Sender::Visitor, &spam, 0, T), Action::Kick);
        assert_eq!(decide(Sender::Visitor, &spam, 1, T), Action::Ban, "second strike");
        assert_eq!(decide(Sender::Visitor, &unsure, 3, T), Action::Flag);
        assert_eq!(decide(Sender::Member, &scam, 5, T), Action::Timeout, "members are never kicked or banned");
        assert_eq!(decide(Sender::Member, &unsure, 0, T), Action::Flag);
        for sender in [Sender::NewVisitor, Sender::Visitor, Sender::Member] {
            assert_eq!(decide(sender, &fine, 9, T), Action::None);
        }
    }

    #[test]
    fn established_members_skip_the_judge_unless_they_post_links() {
        let now = Utc::now();
        let members = vec!["builder".to_owned()];
        let mut m = Incoming {
            guild_id: "g".into(),
            message_id: "1".into(),
            channel_id: "2".into(),
            author_id: "175928847299117063".into(),
            username: "m".into(),
            content: "anyone tried the new agent SDK?".into(),
            author_is_bot: false,
            joined_at: Some(now - Duration::days(30)),
            role_ids: vec!["builder".into()],
            mentions_everyone: false,
        };
        assert_eq!(sender(&m, &members, now), Sender::Member);
        assert!(!needs_judging(&m, Sender::Member, now));
        m.content = "check https://example.com".into();
        assert!(needs_judging(&m, Sender::Member, now));
        m.content = "hi".into();
        m.joined_at = Some(now - Duration::days(2));
        assert!(needs_judging(&m, Sender::Member, now), "new members are judged");
        m.role_ids.clear();
        assert_eq!(sender(&m, &members, now), Sender::NewVisitor);
        m.joined_at = Some(now - Duration::days(20));
        assert_eq!(sender(&m, &members, now), Sender::Visitor);
        m.content = "   ".into();
        assert!(!needs_judging(&m, Sender::Visitor, now));
        m.role_ids = vec!["mod".into()];
        assert!(is_staff(&m, &["mod".to_owned()]));
    }

    #[test]
    fn snowflakes_and_links() {
        assert_eq!(created_at("175928847299117063").unwrap().format("%Y-%m-%d").to_string(), "2016-04-30");
        assert!(has_link("free nitro at discord.gg/abc"));
        assert!(has_link("HTTPS://x.y"));
        assert!(!has_link("I use https less than I should"));
    }
}
