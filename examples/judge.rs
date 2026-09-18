//! Ask the real Jev about some messages, the way the bot does:
//!
//!     cargo run --example judge -- "free nitro at https://gift.example" "anyone tried the new agent SDK?"
//!
//! Reads TYPESAFE_API_KEY from the environment or ./.env.

use chrono::Utc;
use jev::AsyncTypeSafeClient;
use jev_discord_bot::{
    judge::{JudgeContext, judge},
    rules::{Decision, Incoming, Thresholds, decide},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let key = std::env::var("TYPESAFE_API_KEY").ok().filter(|k| !k.trim().is_empty()).or_else(|| {
        std::fs::read_to_string(".env").ok()?.lines().find_map(|l| l.strip_prefix("TYPESAFE_API_KEY=").map(|v| v.split('#').next().unwrap_or("").trim().to_owned())).filter(|k| !k.is_empty())
    });
    let Some(key) = key else { anyhow::bail!("set TYPESAFE_API_KEY (or put it in .env)") };
    let client = AsyncTypeSafeClient::from_key(&key).map_err(|e| anyhow::anyhow!("{e}"))?;
    let thresholds = Thresholds { confident: 0.9, flag: 0.6 };
    for text in std::env::args().skip(1) {
        let incoming = Incoming {
            guild_id: "test".into(),
            message_id: "1".into(),
            channel_id: "general".into(),
            author_id: "1200000000000000000".into(),
            username: "tester".into(),
            content: text.clone(),
            author_is_bot: false,
            joined_at: Some(Utc::now() - chrono::Duration::days(2)),
            role_ids: vec![],
            mentions_everyone: false,
        };
        let context = JudgeContext { community: "An AI dev community.", channel: "general", prior_messages: 3 };
        let (verdict, evaluation) = judge(&client, &incoming, context, Utc::now()).await?;
        let decision = match decide(&verdict, thresholds) {
            Decision::Offense => "OFFENSE",
            Decision::Review => "review",
            Decision::Fine => "fine",
        };
        println!(
            "{decision:8} {:>4.0}% bad  lure {:>3.0}%  {:<22} {} tokens  | {text}",
            verdict.bad() * 100.0,
            verdict.lure * 100.0,
            verdict.kind,
            evaluation.usage.input_tokens + evaluation.usage.output_tokens
        );
    }
    Ok(())
}
