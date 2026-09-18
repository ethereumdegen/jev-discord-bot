//! Buttons on log-channel posts (Undo, Dismiss, Take action) and the /guard
//! command, answered through Discord. Only mods may press; only managers switch modes.

use anyhow::Result;
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    discord::{PERM_ADMINISTRATOR, PERM_BAN_MEMBERS, PERM_MANAGE_GUILD, PERM_MODERATE_MEMBERS},
    engine, guilds,
    hot::{month, usage_key},
    state::AppState,
};

#[derive(Debug, Clone)]
pub struct InteractionIn {
    pub id: String,
    pub token: String,
    pub guild_id: Option<String>,
    pub user_id: String,
    /// The member's resolved permissions in the channel.
    pub permissions: u64,
    pub kind: InteractionKind,
}

#[derive(Debug, Clone)]
pub enum InteractionKind {
    Button { custom_id: String, message_content: String },
    Command { subcommand: String, value: Option<String> },
    Other,
}

pub fn commands() -> Value {
    json!([{
        "name": "guard",
        "description": "Degen Guard, the spam bot",
        "default_member_permissions": PERM_MANAGE_GUILD.to_string(),
        "dm_permission": false,
        "options": [
            { "type": 1, "name": "status", "description": "What the bot has done this month" },
            { "type": 1, "name": "mode", "description": "Watch, enforce, or pause",
              "options": [{ "type": 3, "name": "value", "description": "The mode", "required": true,
                            "choices": [{ "name": "watch (log only)", "value": "watch" }, { "name": "enforce (act)", "value": "enforce" }, { "name": "paused", "value": "paused" }] }] }
        ]
    }])
}

pub async fn handle(state: &AppState, interaction: &InteractionIn) -> Result<()> {
    let body = answer(state, interaction).await?;
    state.discord.respond(&interaction.id, &interaction.token, body).await
}

/// The response body; separate so tests can read it.
pub async fn answer(state: &AppState, interaction: &InteractionIn) -> Result<Value> {
    let private = |text: &str| json!({ "type": 4, "data": { "content": text, "flags": 64 } });
    let Some(guild_id) = interaction.guild_id.as_deref() else { return Ok(private("This works in a server.")) };
    match &interaction.kind {
        InteractionKind::Button { custom_id, message_content } => {
            if interaction.permissions & (PERM_ADMINISTRATOR | PERM_BAN_MEMBERS | PERM_MODERATE_MEMBERS) == 0 {
                return Ok(private("Only mods can do that."));
            }
            let (verb, id) = match custom_id.split(':').collect::<Vec<_>>().as_slice() {
                ["g", verb, id] => (verb.to_string(), id.parse::<Uuid>().ok()),
                _ => (String::new(), None),
            };
            let Some(id) = id else { return Ok(private("I don't know that button.")) };
            let did = match verb.as_str() {
                "undo" => engine::undo(state, guild_id, id, &format!("discord:{}", interaction.user_id)).await?,
                "confirm" => engine::confirm(state, guild_id, id).await?,
                _ => None,
            };
            let Some(did) = did else { return Ok(private("Already handled.")) };
            // Update the post in place: who did what, and no more buttons.
            Ok(json!({ "type": 7, "data": { "content": format!("{message_content}\n↳ <@{}>: {did}", interaction.user_id), "components": [], "allowed_mentions": { "parse": [] } } }))
        }
        InteractionKind::Command { subcommand, value } => {
            if interaction.permissions & (PERM_ADMINISTRATOR | PERM_MANAGE_GUILD) == 0 {
                return Ok(private("Only server managers can do that."));
            }
            match (subcommand.as_str(), value.as_deref()) {
                ("mode", Some(mode @ ("watch" | "enforce" | "paused"))) => {
                    let updated = sqlx::query("UPDATE guilds SET mode=$2,updated_at=now() WHERE id=$1").bind(guild_id).bind(mode).execute(&state.pool).await?;
                    if updated.rows_affected() == 0 {
                        return Ok(private("I don't know this server yet; try again in a minute."));
                    }
                    guilds::forget(&state.hot, guild_id).await?;
                    Ok(private(match mode {
                        "enforce" => "Enforcing: confirmed spam gets the next step on your ladder.",
                        "watch" => "Watching: I'll only log what I would do.",
                        _ => "Paused: I'm ignoring messages.",
                    }))
                }
                ("status", _) => Ok(private(&status(state, guild_id).await?)),
                _ => Ok(private("Use /guard mode or /guard status.")),
            }
        }
        InteractionKind::Other => Ok(private("I don't handle that.")),
    }
}

pub async fn status(state: &AppState, guild_id: &str) -> Result<String> {
    let Some(settings) = guilds::load_fresh(&state.pool, guild_id).await? else { return Ok("I don't know this server yet.".into()) };
    let (flagged, struck, banned): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE outcome='review'),count(*) FILTER (WHERE outcome<>'review'),count(*) FILTER (WHERE outcome='ban' AND enforced)
         FROM actions WHERE guild_id=$1 AND created_at>=date_trunc('month',now())",
    )
    .bind(guild_id)
    .fetch_one(&state.pool)
    .await?;
    let used = state.hot.counter(&usage_key(guild_id, &month(Utc::now()))).await?;
    Ok(format!(
        "Mode: **{}**. This month: {used}/{} messages judged, {struck} strikes ({banned} bans), {flagged} for review. Settings and the full log: {}/servers/{guild_id}",
        settings.guild.mode, settings.guild.monthly_allowance, state.config.base_url
    ))
}
