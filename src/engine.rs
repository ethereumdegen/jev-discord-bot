//! What happens to one message, in any server: skip or judge, then a review
//! entry or a strike on the server's ladder, the action itself (unless the
//! server is in watch mode), the audit-log row, and the log-channel post.
//! Also confirming a review entry and undoing an action.

use anyhow::Result;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    discord::Outcome,
    guilds::{self, Settings},
    hot::{month, usage_key},
    judge::{JudgeContext, judge},
    rules::{self, Decision, Incoming, Step, Verdict},
    state::AppState,
};

const NOTICE_SECONDS: u64 = 60;

/// Everything about one message. Returns the audit-log row's id when the
/// message was flagged or punished.
pub async fn handle_message(state: &AppState, incoming: &Incoming) -> Result<Option<Uuid>> {
    if incoming.author_is_bot {
        return Ok(None);
    }
    let Some(settings) = guilds::load(&state.pool, &state.hot, &incoming.guild_id).await? else { return Ok(None) };
    let guild = &settings.guild;
    if guild.removed_at.is_some() || guild.mode == "paused" || !settings.spam.enabled {
        return Ok(None);
    }
    let exempt = if guild.owner_user_id.as_deref() == Some(incoming.author_id.as_str()) {
        Some("server owner")
    } else if guild.exempt_channel_ids.contains(&incoming.channel_id) {
        Some("exempt channel")
    } else if incoming.role_ids.iter().any(|r| guild.exempt_role_ids.contains(r)) {
        Some("exempt role")
    } else {
        None
    };
    // Exempt people are still judged, so mods can see what the bot would
    // have done; nothing is ever done to them.
    if let Some(why) = exempt {
        tracing::info!(guild = %incoming.guild_id, why, "exempt: judged but never acted on");
    }
    if !rules::needs_judging(incoming) {
        return Ok(None);
    }
    let seen = state.hot.incr(&format!("jev:msgs:{}:{}", incoming.guild_id, incoming.author_id), 90 * 86_400).await?;
    let now = Utc::now();
    let this_month = month(now);
    let used = state.hot.incr(&usage_key(&incoming.guild_id, &this_month), 40 * 86_400).await?;
    if used > i64::from(guild.monthly_allowance) {
        if let Some(log) = &guild.log_channel_id
            && state.hot.first_in(&format!("jev:quota-note:{}:{this_month}", incoming.guild_id), 40 * 86_400).await?
        {
            let text = format!(
                "⚠️ {} has used this month's {} judged messages, so it's only watching until the 1st. Ask for a bigger allowance at {}.",
                state.config.brand, guild.monthly_allowance, state.config.base_url
            );
            let _ = state.discord.post(log, &text, None, None).await;
        }
        return Ok(None);
    }

    let channel = channel_name(state, &incoming.channel_id).await;
    let context = JudgeContext { community: &guild.community, channel: &channel, prior_messages: seen - 1 };
    let (verdict, evaluation) = judge(&state.jev, incoming, context, now).await?;
    let record = Record { settings: &settings, incoming, channel: &channel, verdict: &verdict, model: &evaluation.model, tokens: (evaluation.usage.input_tokens, evaluation.usage.output_tokens) };
    let decision = rules::decide(&verdict, settings.thresholds());
    tracing::info!(guild = %incoming.guild_id, kind = %verdict.kind, bad = verdict.bad(), lure = verdict.lure, ?decision, exempt, "judged");
    if let Some(why) = exempt
        && decision != Decision::Fine
    {
        let (outcome, n) = match decision {
            Decision::Review => ("review", None),
            _ => {
                let (step, n) = next_step(state, &settings, &incoming.author_id).await?;
                (step.as_str(), Some(n))
            }
        };
        let applied = Applied { exempt: Some(why.to_owned()), ..Applied::default() };
        let id = insert(state, &record, outcome, n, &applied).await?;
        if let Some(id) = id {
            report(state, &settings, id, incoming, &channel, &verdict, outcome, n, &applied).await;
        }
        return Ok(id);
    }
    match decision {
        Decision::Fine => Ok(None),
        Decision::Review => {
            let id = insert(state, &record, "review", None, &Applied::default()).await?;
            if let Some(id) = id {
                report(state, &settings, id, incoming, &channel, &verdict, "review", None, &Applied::default()).await;
            }
            Ok(id)
        }
        Decision::Offense => {
            let (step, n, applied) = strike(state, &settings, &incoming.author_id, &incoming.channel_id, &incoming.message_id).await?;
            let Some(id) = insert(state, &record, step.as_str(), Some(n), &applied).await? else { return Ok(None) };
            after_strike(state, &settings, id, incoming, &channel, &verdict, step, n, &applied).await?;
            Ok(Some(id))
        }
    }
}

async fn channel_name(state: &AppState, channel_id: &str) -> String {
    let key = format!("jev:chan:{channel_id}");
    if let Ok(Some(name)) = state.hot.get(&key).await {
        return name;
    }
    match state.discord.channel_name(channel_id).await {
        Some(name) => {
            let _ = state.hot.set_ex(&key, &name, 86_400).await;
            name
        }
        None => channel_id.to_owned(),
    }
}

struct Record<'a> {
    settings: &'a Settings,
    incoming: &'a Incoming,
    channel: &'a str,
    verdict: &'a Verdict,
    model: &'a str,
    tokens: (u64, u64),
}

/// What carrying out a step came to.
#[derive(Debug, Default, Clone)]
pub struct Applied {
    /// It really happened (enforce mode, and Discord allowed it).
    pub enforced: bool,
    pub message_deleted: bool,
    /// "Couldn't act": why Discord refused.
    pub error: Option<String>,
    /// Never acted on: the author is the owner, or in an exempt role or channel.
    pub exempt: Option<String>,
}

/// Work out which strike this is and carry out its step (unless the server is watching).
async fn strike(state: &AppState, settings: &Settings, user_id: &str, channel_id: &str, message_id: &str) -> Result<(Step, i32, Applied)> {
    let guild = &settings.guild;
    let (step, n) = next_step(state, settings, user_id).await?;
    if guild.mode != "enforce" {
        return Ok((step, n, Applied::default()));
    }
    let mut applied = Applied { enforced: true, ..Applied::default() };
    if step.deletes() {
        match state.discord.delete_message(channel_id, message_id).await? {
            Outcome::Done => applied.message_deleted = true,
            Outcome::Refused(why) => applied.error = Some(format!("couldn't delete the message: {why}")),
        }
    }
    let outcome = match step {
        Step::Timeout => Some(state.discord.timeout(&guild.id, user_id, Some(Utc::now() + Duration::minutes(i64::from(guild.timeout_minutes)))).await?),
        Step::Kick => Some(state.discord.kick(&guild.id, user_id).await?),
        Step::Ban => Some(state.discord.ban(&guild.id, user_id).await?),
        Step::None | Step::Warn => None,
    };
    if let Some(Outcome::Refused(why)) = outcome {
        applied.enforced = false;
        applied.error = Some(format!("couldn't {}: {why} (the bot's role must be above theirs, and it can't touch the owner or admins)", step.as_str()));
    }
    Ok((step, n, applied))
}

/// Which strike this would be, and its step, without doing anything.
async fn next_step(state: &AppState, settings: &Settings, user_id: &str) -> Result<(Step, i32)> {
    let live: i64 = sqlx::query_scalar("SELECT count(*) FROM strikes WHERE guild_id=$1 AND user_id=$2 AND cleared_at IS NULL AND expires_at>now()")
        .bind(&settings.guild.id)
        .bind(user_id)
        .fetch_one(&state.pool)
        .await?;
    let n = live as i32 + 1;
    Ok((rules::step_for(&settings.ladder(), n as usize), n))
}

/// The strike row, the warning notice and the log post, once the action row exists.
#[allow(clippy::too_many_arguments)]
async fn after_strike(state: &AppState, settings: &Settings, id: Uuid, incoming: &Incoming, channel: &str, verdict: &Verdict, step: Step, n: i32, applied: &Applied) -> Result<()> {
    if applied.enforced {
        sqlx::query("INSERT INTO strikes(id,guild_id,user_id,action_id,expires_at) VALUES($1,$2,$3,$4,now()+make_interval(days=>$5))")
            .bind(Uuid::new_v4())
            .bind(&settings.guild.id)
            .bind(&incoming.author_id)
            .bind(id)
            .bind(settings.guild.strike_days)
            .execute(&state.pool)
            .await?;
        if step == Step::Warn {
            let next = rules::step_for(&settings.ladder(), n as usize + 1);
            let text = format!(
                "<@{}> your message was removed by {} because it looked like spam. Next time: {}.",
                incoming.author_id,
                state.config.brand,
                next.describe()
            );
            if let Ok(notice) = state.discord.post(&incoming.channel_id, &text, None, Some(&incoming.author_id)).await {
                let discord = state.discord.clone();
                let channel_id = incoming.channel_id.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(NOTICE_SECONDS)).await;
                    let _ = discord.delete_message(&channel_id, &notice).await;
                });
            }
        }
    }
    report(state, settings, id, incoming, channel, verdict, step.as_str(), Some(n), applied).await;
    Ok(())
}

async fn insert(state: &AppState, record: &Record<'_>, outcome: &str, strike_number: Option<i32>, applied: &Applied) -> Result<Option<Uuid>> {
    let id = Uuid::new_v4();
    let inserted = sqlx::query(
        "INSERT INTO actions(id,guild_id,rule_id,author_id,username,channel_id,channel_name,message_id,excerpt,verdict,probabilities,confidence,lure,
           outcome,strike_number,enforced,message_deleted,error,model,input_tokens,output_tokens)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21) ON CONFLICT(message_id) DO NOTHING",
    )
    .bind(id)
    .bind(&record.settings.guild.id)
    .bind(record.settings.spam.id)
    .bind(&record.incoming.author_id)
    .bind(&record.incoming.username)
    .bind(&record.incoming.channel_id)
    .bind(record.channel)
    .bind(&record.incoming.message_id)
    .bind(record.incoming.content.chars().take(500).collect::<String>())
    .bind(&record.verdict.kind)
    .bind(json!(record.verdict.probabilities))
    .bind(record.verdict.confidence)
    .bind(record.verdict.lure)
    .bind(outcome)
    .bind(strike_number)
    .bind(applied.enforced)
    .bind(applied.message_deleted)
    .bind(&applied.error)
    .bind(record.model)
    .bind(i32::try_from(record.tokens.0).unwrap_or(i32::MAX))
    .bind(i32::try_from(record.tokens.1).unwrap_or(i32::MAX))
    .execute(&state.pool)
    .await?;
    Ok((inserted.rows_affected() > 0).then_some(id))
}

/// Post to the server's log channel, with buttons. Never fails the caller.
#[allow(clippy::too_many_arguments)]
async fn report(state: &AppState, settings: &Settings, id: Uuid, incoming: &Incoming, channel: &str, verdict: &Verdict, outcome: &str, n: Option<i32>, applied: &Applied) {
    let Some(log) = &settings.guild.log_channel_id else { return };
    let (content, components) = report_body(id, incoming, channel, verdict, outcome, n, applied, &settings.guild.mode);
    if let Err(error) = state.discord.post(log, &content, Some(components), None).await {
        tracing::warn!(?error, guild = %settings.guild.id, "could not post to the log channel");
    }
}

#[allow(clippy::too_many_arguments)]
pub fn report_body(id: Uuid, incoming: &Incoming, channel: &str, verdict: &Verdict, outcome: &str, n: Option<i32>, applied: &Applied, mode: &str) -> (String, Value) {
    let did = match (outcome, applied.enforced, &applied.error) {
        _ if applied.exempt.is_some() => format!("[exempt: {}, nothing done] would **{outcome}**", applied.exempt.as_deref().unwrap_or_default()),
        ("review", _, _) => "⚑ **review**".to_owned(),
        (step, _, Some(error)) => format!("⚠️ tried to **{step}** but {error}"),
        (step, true, None) => format!("**{step}**"),
        (step, false, None) if mode == "watch" => format!("[watching] would **{step}**"),
        (step, false, None) => format!("would **{step}**"),
    };
    let strike = n.map(|n| format!(" · strike {n}")).unwrap_or_default();
    let excerpt: String = incoming.content.chars().take(300).collect::<String>().replace('`', "'");
    let content = format!(
        "{did} <@{}> in #{channel}: {} · {:.0}% sure{strike}\n`{excerpt}`",
        incoming.author_id,
        verdict.kind,
        verdict.bad() * 100.0
    );
    let buttons = if outcome == "review" {
        json!([
            { "type": 2, "style": 4, "label": "Take action", "custom_id": format!("g:confirm:{id}") },
            { "type": 2, "style": 2, "label": "Dismiss", "custom_id": format!("g:undo:{id}") }
        ])
    } else {
        let label = if applied.enforced && matches!(outcome, "ban" | "timeout" | "kick") { "Undo" } else { "Wrong call" };
        json!([{ "type": 2, "style": 2, "label": label, "custom_id": format!("g:undo:{id}") }])
    };
    (content, json!([{ "type": 1, "components": buttons }]))
}

/// A mod reverses an action or dismisses a review entry: lift a ban or a
/// time-out, clear the strike, and mark the verdict wrong. Returns what it did.
pub async fn undo(state: &AppState, guild_id: &str, action_id: Uuid, by: &str) -> Result<Option<String>> {
    let row: Option<(String, String, bool)> = sqlx::query_as("SELECT author_id,outcome,enforced FROM actions WHERE id=$1 AND guild_id=$2 AND reversed_at IS NULL")
        .bind(action_id)
        .bind(guild_id)
        .fetch_optional(&state.pool)
        .await?;
    let Some((author_id, outcome, enforced)) = row else { return Ok(None) };
    let mut did = if outcome == "review" { "dismissed".to_owned() } else { "marked as a wrong call".to_owned() };
    if enforced {
        match outcome.as_str() {
            "ban" => {
                state.discord.unban(guild_id, &author_id).await?;
                did = "unbanned; strike cleared".into();
            }
            "timeout" => {
                state.discord.timeout(guild_id, &author_id, None).await?;
                did = "time-out lifted; strike cleared".into();
            }
            "kick" => did = "strike cleared (they can rejoin with an invite)".into(),
            _ => did = "strike cleared".into(),
        }
    }
    sqlx::query("UPDATE strikes SET cleared_at=now() WHERE action_id=$1 AND cleared_at IS NULL").bind(action_id).execute(&state.pool).await?;
    sqlx::query("UPDATE actions SET reversed_at=now(),reversed_by=$2,marked_wrong=true WHERE id=$1").bind(action_id).bind(by).execute(&state.pool).await?;
    Ok(Some(did))
}

#[derive(sqlx::FromRow)]
struct ReviewRow {
    author_id: String,
    username: String,
    channel_id: String,
    message_id: String,
    excerpt: String,
    channel: String,
    kind: String,
    confidence: f64,
    lure: f64,
    probabilities: Value,
}

/// A mod agrees with a review entry: treat it as a confirmed offense now.
pub async fn confirm(state: &AppState, guild_id: &str, action_id: Uuid) -> Result<Option<String>> {
    let row: Option<ReviewRow> = sqlx::query_as(
        "SELECT author_id,username,channel_id,message_id,excerpt,COALESCE(channel_name,channel_id) AS channel,verdict AS kind,confidence,lure,probabilities
         FROM actions WHERE id=$1 AND guild_id=$2 AND outcome='review' AND reversed_at IS NULL",
    )
    .bind(action_id)
    .bind(guild_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(ReviewRow { author_id, username, channel_id, message_id, excerpt, channel, kind, confidence, lure, probabilities }) = row else { return Ok(None) };
    let Some(settings) = guilds::load_fresh(&state.pool, guild_id).await? else { return Ok(None) };
    let (step, n, applied) = strike(state, &settings, &author_id, &channel_id, &message_id).await?;
    sqlx::query("UPDATE actions SET outcome=$2,strike_number=$3,enforced=$4,message_deleted=$5,error=$6 WHERE id=$1")
        .bind(action_id)
        .bind(step.as_str())
        .bind(n)
        .bind(applied.enforced)
        .bind(applied.message_deleted)
        .bind(&applied.error)
        .execute(&state.pool)
        .await?;
    let incoming = Incoming {
        guild_id: guild_id.to_owned(),
        message_id,
        channel_id,
        author_id,
        username,
        content: excerpt,
        author_is_bot: false,
        joined_at: None,
        role_ids: vec![],
        mentions_everyone: false,
    };
    let verdict = Verdict { kind, probabilities: serde_json::from_value(probabilities).unwrap_or_default(), confidence, lure };
    after_strike(state, &settings, action_id, &incoming, &channel, &verdict, step, n, &applied).await?;
    let done = match (&applied.error, applied.enforced) {
        (Some(error), _) => format!("tried to {} but {error}", step.as_str()),
        (None, true) => format!("{} (strike {n})", step.as_str()),
        (None, false) => format!("would {} (strike {n}); the server is watching", step.as_str()),
    };
    Ok(Some(done))
}
