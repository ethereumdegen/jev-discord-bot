//! Asking Jev (TypeSafe's typed judgments) about one message. The context goes
//! in the State, since every answer is judged against it.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use jev::{Answer, AsyncTypeSafeClient, Evaluation, Question, Questions, State};

use crate::rules::{Incoming, Verdict, created_at, has_link};

pub const DEFAULT_COMMUNITY: &str = "A Discord server. Conversation, questions, and sharing your own work where it fits are normal.";

pub struct JudgeContext<'a> {
    /// The owner's description of their server.
    pub community: &'a str,
    pub channel: &'a str,
    pub prior_messages: i64,
}

pub async fn judge(client: &AsyncTypeSafeClient, incoming: &Incoming, context: JudgeContext<'_>, now: DateTime<Utc>) -> Result<(Verdict, Evaluation)> {
    let days = |at: Option<DateTime<Utc>>| at.map(|at| (now - at).num_days().to_string()).unwrap_or_else(|| "unknown".into());
    let account_days = days(created_at(&incoming.author_id));
    let joined_days = days(incoming.joined_at);
    let prior = context.prior_messages.to_string();
    let community = if context.community.trim().is_empty() { DEFAULT_COMMUNITY } else { context.community };
    let facts = [
        ("community", community),
        ("channel", context.channel),
        ("account_age_days", account_days.as_str()),
        ("days_in_server", joined_days.as_str()),
        ("earlier_messages_seen", prior.as_str()),
        ("has_link", if has_link(&incoming.content) { "yes" } else { "no" }),
    ];
    let state = State::detailed(incoming.content.chars().take(2000).collect::<String>(), &facts);
    let mut questions = Questions::new();
    questions.insert(
        "kind".into(),
        Question::choice(
            "What is this Discord message?",
            &[
                ("legit", "An ordinary message: conversation, a question, help, or sharing work that fits the channel."),
                ("spam", "Unwanted bulk or repetitive content, advertising, or nonsense posted to be seen."),
                ("scam_or_phishing", "Tries to steal accounts, money or crypto: fake giveaways, fake support, 'DM me', suspicious links, impersonation."),
                ("self_promo_off_topic", "Promotes the sender's own product, server or service where it doesn't belong."),
            ],
        ),
    );
    questions.insert("lure".into(), Question::yes_no("Does this message try to get people to click a link, message someone privately, or pay?"));
    let evaluation = client.evaluate(&state, questions).await.context("Jev could not judge the message")?;
    let Some(Answer::Choice { choice, probabilities, confidence }) = evaluation.answers.get("kind") else {
        bail!("Jev's answer had no choice for `kind`");
    };
    let lure = match evaluation.answers.get("lure") {
        Some(Answer::Noul { noul }) => *noul,
        _ => 0.0,
    };
    let verdict = Verdict { kind: choice.clone(), probabilities: probabilities.clone(), confidence: *confidence, lure };
    Ok((verdict, evaluation))
}
