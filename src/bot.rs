//! What the bot does with each message and each mod interaction. main.rs only
//! turns gateway events into `Incoming` / `InteractionIn` and calls in here,
//! so all of this runs in tests against stand-ins.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use jev::AsyncTypeSafeClient;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    config::Config,
    discord::Discord,
    judge::{JudgeContext, judge},
    rules::{self, Action, Incoming, MEMBER_TIMEOUT_HOURS, Verdict},
};

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

// Discord permission bits.
const ADMINISTRATOR: u64 = 1 << 3;
const BAN_MEMBERS: u64 = 1 << 2;
const MANAGE_GUILD: u64 = 1 << 5;
const MODERATE_MEMBERS: u64 = 1 << 40;

pub struct Bot {
    pub config: Config,
    pub pool: PgPool,
    pub discord: Discord,
    pub jev: AsyncTypeSafeClient,
}

impl Bot {
    pub fn new(config: Config, pool: PgPool) -> Result<Self> {
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(20)).build()?;
        let discord = Discord::new(http, &config.discord_api_base, &config.bot_token, &config.guild_id);
        let mut jev = AsyncTypeSafeClient::from_key(&config.typesafe_api_key).map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Some(endpoint) = &config.typesafe_endpoint {
            jev = jev.with_endpoint(endpoint.clone());
        }
        Ok(Self { config, pool, discord, jev })
    }

    pub async fn mode(&self) -> Result<String> {
        let stored: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE guild_id=$1 AND key='mode'")
            .bind(&self.config.guild_id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(stored.unwrap_or_else(|| self.config.default_mode.clone()))
    }

    /// Everything about one message: skip, judge, decide, act, log. Returns the
    /// logged action's id when something was worth logging.
    pub async fn handle_message(&self, incoming: &Incoming) -> Result<Option<Uuid>> {
        if incoming.author_is_bot || incoming.guild_id != self.config.guild_id || rules::is_staff(incoming, &self.config.staff_role_ids) {
            return Ok(None);
        }
        let (messages, strikes, last_judged): (i32, i32, Option<DateTime<Utc>>) = sqlx::query_as(
            "INSERT INTO authors(guild_id,author_id,messages) VALUES($1,$2,1)
             ON CONFLICT(guild_id,author_id) DO UPDATE SET messages=authors.messages+1
             RETURNING messages,strikes,last_judged_at",
        )
        .bind(&incoming.guild_id)
        .bind(&incoming.author_id)
        .fetch_one(&self.pool)
        .await?;
        let now = Utc::now();
        let sender = rules::sender(incoming, &self.config.member_role_ids, now);
        if !rules::needs_judging(incoming, sender, now) {
            return Ok(None);
        }
        // A flood is judged once every few seconds, not once per message; links always are.
        if last_judged.is_some_and(|at| now - at < Duration::seconds(5)) && !rules::has_link(&incoming.content) {
            return Ok(None);
        }
        sqlx::query("UPDATE authors SET last_judged_at=now() WHERE guild_id=$1 AND author_id=$2")
            .bind(&incoming.guild_id)
            .bind(&incoming.author_id)
            .execute(&self.pool)
            .await?;
        let channel = self.discord.channel_name(&incoming.channel_id).await.unwrap_or_else(|| incoming.channel_id.clone());
        let context = JudgeContext { community: &self.config.community, channel: &channel, sender, prior_messages: messages - 1 };
        let (verdict, evaluation) = judge(&self.jev, incoming, context, now).await?;
        let action = rules::decide(sender, &verdict, strikes, self.config.thresholds);
        if action == Action::None {
            return Ok(None);
        }
        let enforce = self.mode().await? == "enforce";
        if enforce {
            self.act(incoming, action).await?;
            if action.strikes() {
                sqlx::query("UPDATE authors SET strikes=strikes+1 WHERE guild_id=$1 AND author_id=$2")
                    .bind(&incoming.guild_id)
                    .bind(&incoming.author_id)
                    .execute(&self.pool)
                    .await?;
            }
        }
        let id = Uuid::new_v4();
        let inserted = sqlx::query(
            "INSERT INTO actions(id,guild_id,author_id,username,channel_id,message_id,excerpt,sender,verdict,probabilities,confidence,lure,action,enforced,model,input_tokens,output_tokens)
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17) ON CONFLICT(message_id) DO NOTHING",
        )
        .bind(id)
        .bind(&incoming.guild_id)
        .bind(&incoming.author_id)
        .bind(&incoming.username)
        .bind(&incoming.channel_id)
        .bind(&incoming.message_id)
        .bind(incoming.content.chars().take(500).collect::<String>())
        .bind(sender.as_str())
        .bind(&verdict.kind)
        .bind(json!(verdict.probabilities))
        .bind(verdict.confidence)
        .bind(verdict.lure)
        .bind(action.as_str())
        .bind(enforce)
        .bind(&evaluation.model)
        .bind(i32::try_from(evaluation.usage.input_tokens).unwrap_or(i32::MAX))
        .bind(i32::try_from(evaluation.usage.output_tokens).unwrap_or(i32::MAX))
        .execute(&self.pool)
        .await?;
        if inserted.rows_affected() == 0 {
            return Ok(None);
        }
        if let Some(log) = &self.config.mod_log_channel_id {
            let (content, components) = report(id, incoming, &channel, sender, &verdict, action, enforce);
            if let Err(error) = self.discord.post(log, &content, Some(components)).await {
                tracing::warn!(?error, "could not post to #mod-log");
            }
        }
        tracing::info!(author = %incoming.author_id, action = action.as_str(), enforce, verdict = %verdict.kind, "moderated a message");
        Ok(Some(id))
    }

    async fn act(&self, incoming: &Incoming, action: Action) -> Result<()> {
        if action.deletes() {
            self.discord.delete_message(&incoming.channel_id, &incoming.message_id).await?;
        }
        match action {
            Action::Timeout => self.discord.timeout(&incoming.author_id, Some(Utc::now() + Duration::hours(MEMBER_TIMEOUT_HOURS))).await,
            Action::Kick => self.discord.kick(&incoming.author_id).await,
            Action::Ban => self.discord.ban(&incoming.author_id).await,
            _ => Ok(()),
        }
    }

    /// A mod reverses an action: lift a ban or a time-out, forgive the strike,
    /// and remember the verdict was wrong. Returns what it did, for the reply.
    pub async fn undo(&self, action_id: Uuid, by: &str) -> Result<Option<String>> {
        let row: Option<(String, String, bool)> = sqlx::query_as("SELECT author_id,action,enforced FROM actions WHERE id=$1 AND guild_id=$2 AND reversed_at IS NULL")
            .bind(action_id)
            .bind(&self.config.guild_id)
            .fetch_optional(&self.pool)
            .await?;
        let Some((author_id, action, enforced)) = row else { return Ok(None) };
        let mut did = "marked as a wrong call".to_owned();
        if enforced {
            match action.as_str() {
                "ban" => {
                    self.discord.unban(&author_id).await?;
                    did = "unbanned and marked as a wrong call".into();
                }
                "timeout" => {
                    self.discord.timeout(&author_id, None).await?;
                    did = "time-out lifted and marked as a wrong call".into();
                }
                "kick" => did = "marked as a wrong call (they can rejoin with an invite)".into(),
                _ => {}
            }
            if matches!(action.as_str(), "ban" | "kick" | "timeout") {
                sqlx::query("UPDATE authors SET strikes=GREATEST(strikes-1,0) WHERE guild_id=$1 AND author_id=$2")
                    .bind(&self.config.guild_id)
                    .bind(&author_id)
                    .execute(&self.pool)
                    .await?;
            }
        }
        sqlx::query("UPDATE actions SET reversed_at=now(),reversed_by=$2,marked_wrong=true WHERE id=$1").bind(action_id).bind(by).execute(&self.pool).await?;
        Ok(Some(did))
    }

    /// A button press or a /jev command, answered through Discord.
    pub async fn handle_interaction(&self, interaction: &InteractionIn) -> Result<()> {
        let answer = self.answer(interaction).await?;
        self.discord.respond(&interaction.id, &interaction.token, answer).await
    }

    /// The response body for an interaction; separate so tests can read it.
    pub async fn answer(&self, interaction: &InteractionIn) -> Result<Value> {
        let private = |text: &str| json!({ "type": 4, "data": { "content": text, "flags": 64 } });
        if interaction.guild_id.as_deref() != Some(self.config.guild_id.as_str()) {
            return Ok(private("This bot only works in its own server."));
        }
        match &interaction.kind {
            InteractionKind::Button { custom_id, message_content } => {
                if interaction.permissions & (ADMINISTRATOR | BAN_MEMBERS | MODERATE_MEMBERS) == 0 {
                    return Ok(private("Only mods can do that."));
                }
                let Some(id) = custom_id.strip_prefix("jev:undo:").and_then(|id| id.parse::<Uuid>().ok()) else {
                    return Ok(private("I don't know that button."));
                };
                let Some(did) = self.undo(id, &interaction.user_id).await? else {
                    return Ok(private("Already undone."));
                };
                // Update the report in place: say who undid it and drop the button.
                Ok(json!({ "type": 7, "data": { "content": format!("{message_content}\n↩️ <@{}>: {did}", interaction.user_id), "components": [], "allowed_mentions": { "parse": [] } } }))
            }
            InteractionKind::Command { subcommand, value } => {
                if interaction.permissions & (ADMINISTRATOR | MANAGE_GUILD) == 0 {
                    return Ok(private("Only server managers can do that."));
                }
                match (subcommand.as_str(), value.as_deref()) {
                    ("mode", Some(mode @ ("shadow" | "enforce"))) => {
                        sqlx::query(
                            "INSERT INTO settings(guild_id,key,value,updated_by) VALUES($1,'mode',$2,$3)
                             ON CONFLICT(guild_id,key) DO UPDATE SET value=excluded.value,updated_by=excluded.updated_by,updated_at=now()",
                        )
                        .bind(&self.config.guild_id)
                        .bind(mode)
                        .bind(&interaction.user_id)
                        .execute(&self.pool)
                        .await?;
                        let text = if mode == "enforce" {
                            "Enforcing: I'll delete, time out, kick and ban. Members are never kicked or banned."
                        } else {
                            "Shadow mode: I'll only report what I would do."
                        };
                        Ok(private(text))
                    }
                    ("status", _) => Ok(private(&self.status().await?)),
                    _ => Ok(private("Use /jev mode shadow|enforce or /jev status.")),
                }
            }
            InteractionKind::Other => Ok(private("I don't handle that.")),
        }
    }

    pub async fn status(&self) -> Result<String> {
        let (logged, wrong, tokens): (i64, i64, i64) = sqlx::query_as(
            "SELECT count(*),count(*) FILTER (WHERE marked_wrong),COALESCE(sum(input_tokens+output_tokens),0)
             FROM actions WHERE guild_id=$1 AND created_at>now()-interval '7 days'",
        )
        .bind(&self.config.guild_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(format!(
            "Mode: **{}**. Last 7 days: {logged} reported, {wrong} undone as wrong calls, {tokens} Jev tokens. Acts at {:.0}% sure, reports from {:.0}%.",
            self.mode().await?,
            self.config.thresholds.confident * 100.0,
            self.config.thresholds.flag * 100.0
        ))
    }
}

/// The #mod-log report and its button.
pub fn report(id: Uuid, incoming: &Incoming, channel: &str, sender: rules::Sender, verdict: &Verdict, action: Action, enforced: bool) -> (String, Value) {
    let did = match (action, enforced) {
        (Action::Flag, _) => "⚑ flagged".to_owned(),
        (action, true) => format!("**{}**", action.as_str()),
        (action, false) => format!("[shadow] would **{}**", action.as_str()),
    };
    let excerpt: String = incoming.content.chars().take(300).collect::<String>().replace('`', "'");
    let content = format!(
        "{did} <@{}> ({}) in #{channel}: {} · {:.0}% bad · lure {:.0}%\n`{excerpt}`",
        incoming.author_id,
        sender.as_str(),
        verdict.kind,
        verdict.bad() * 100.0,
        verdict.lure * 100.0
    );
    let label = if enforced && matches!(action, Action::Ban | Action::Timeout | Action::Kick) { "Undo" } else { "Wrong call" };
    let components = json!([{ "type": 1, "components": [{ "type": 2, "style": 2, "label": label, "custom_id": format!("jev:undo:{id}") }] }]);
    (content, components)
}

/// The /jev command, registered in the guild at startup. Hidden from everyone
/// without Manage Server.
pub fn commands() -> Value {
    json!([{
        "name": "jev",
        "description": "The spam bot",
        "default_member_permissions": MANAGE_GUILD.to_string(),
        "options": [
            { "type": 1, "name": "status", "description": "What the bot has done this week" },
            { "type": 1, "name": "mode", "description": "Report only, or act",
              "options": [{ "type": 3, "name": "value", "description": "shadow or enforce", "required": true,
                            "choices": [{ "name": "shadow (report only)", "value": "shadow" }, { "name": "enforce (act)", "value": "enforce" }] }] }
        ]
    }])
}

/// An interaction, stripped to what the bot needs.
#[derive(Debug, Clone)]
pub struct InteractionIn {
    pub id: String,
    pub token: String,
    pub guild_id: Option<String>,
    pub user_id: String,
    /// The pressing member's resolved permissions in the channel.
    pub permissions: u64,
    pub kind: InteractionKind,
}

#[derive(Debug, Clone)]
pub enum InteractionKind {
    Button { custom_id: String, message_content: String },
    Command { subcommand: String, value: Option<String> },
    Other,
}

pub async fn connect(url: &str) -> Result<PgPool> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(url)
        .await
        .context("connect to Postgres")?;
    MIGRATOR.run(&pool).await.context("migrate")?;
    Ok(pool)
}
