//! The Discord gateway for every server the bot is in. It never waits on Jev:
//! messages go onto the Redis stream for the workers. Server joins and
//! leaves update Postgres; button presses and /guard are answered here.

use anyhow::Result;
use chrono::{DateTime, Utc};
use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_model::{
    application::interaction::{Interaction, InteractionData, application_command::CommandOptionValue},
    channel::Message,
    gateway::payload::incoming::GuildCreate,
};

use crate::{
    guilds,
    interactions::{self, InteractionIn, InteractionKind},
    rules::Incoming,
    state::AppState,
};

pub async fn run(state: AppState) -> Result<()> {
    // The gateway's TLS needs a process-wide crypto provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let intents = Intents::GUILDS | Intents::GUILD_MESSAGES | Intents::MESSAGE_CONTENT;
    let mut shard = Shard::new(ShardId::ONE, state.config.discord.bot_token.clone(), intents);
    let wanted = EventTypeFlags::READY | EventTypeFlags::GUILD_CREATE | EventTypeFlags::GUILD_DELETE | EventTypeFlags::MESSAGE_CREATE | EventTypeFlags::INTERACTION_CREATE;
    tracing::info!("gateway connecting");
    while let Some(item) = shard.next_event(wanted).await {
        let event = match item {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(?error, "gateway error");
                continue;
            }
        };
        let state = state.clone();
        match event {
            Event::Ready(ready) => {
                tracing::info!(user = %ready.user.name, guilds = ready.guilds.len(), "ready");
                tokio::spawn(async move {
                    if let Err(error) = state.discord.register_commands(interactions::commands()).await {
                        tracing::warn!(?error, "could not register /guard");
                    }
                });
            }
            Event::GuildCreate(created) => {
                if let GuildCreate::Available(guild) = *created {
                    tokio::spawn(async move {
                        let icon = guild.icon.map(|i| i.to_string());
                        let result = guilds::installed(
                            &state.pool,
                            &state.hot,
                            &guild.id.to_string(),
                            &guild.name,
                            icon.as_deref(),
                            Some(&guild.owner_id.to_string()),
                            state.config.default_allowance,
                        )
                        .await;
                        if let Err(error) = result {
                            tracing::warn!(?error, "could not record a server");
                        }
                    });
                }
            }
            Event::GuildDelete(deleted) => {
                // unavailable=true is an outage, not a removal.
                if deleted.unavailable != Some(true) {
                    tokio::spawn(async move {
                        if let Err(error) = guilds::removed(&state.pool, &state.hot, &deleted.id.to_string()).await {
                            tracing::warn!(?error, "could not record a removal");
                        }
                    });
                }
            }
            Event::MessageCreate(message) => {
                if message.author.bot || message.guild_id.is_none() {
                    continue;
                }
                let incoming = incoming(&message.0);
                tokio::spawn(async move {
                    let payload = serde_json::to_string(&incoming).unwrap_or_default();
                    if let Err(error) = state.hot.enqueue(&incoming.message_id, &payload).await {
                        tracing::warn!(?error, "could not queue a message");
                    }
                });
            }
            Event::InteractionCreate(interaction) => {
                let interaction = interaction_in(&interaction.0);
                tokio::spawn(async move {
                    if let Err(error) = interactions::handle(&state, &interaction).await {
                        tracing::warn!(error = %format!("{error:#}"), "could not answer an interaction");
                    }
                });
            }
            _ => {}
        }
    }
    Ok(())
}

fn incoming(message: &Message) -> Incoming {
    let member = message.member.as_ref();
    Incoming {
        guild_id: message.guild_id.map(|g| g.to_string()).unwrap_or_default(),
        message_id: message.id.to_string(),
        channel_id: message.channel_id.to_string(),
        author_id: message.author.id.to_string(),
        username: message.author.name.clone(),
        content: message.content.clone(),
        author_is_bot: message.author.bot,
        joined_at: member.and_then(|m| m.joined_at).and_then(|t| DateTime::<Utc>::from_timestamp_micros(t.as_micros())),
        role_ids: member.map(|m| m.roles.iter().map(ToString::to_string).collect()).unwrap_or_default(),
        mentions_everyone: message.mention_everyone,
    }
}

fn interaction_in(interaction: &Interaction) -> InteractionIn {
    let kind = match &interaction.data {
        Some(InteractionData::MessageComponent(data)) => InteractionKind::Button {
            custom_id: data.custom_id.clone(),
            message_content: interaction.message.as_ref().map(|m| m.content.clone()).unwrap_or_default(),
        },
        Some(InteractionData::ApplicationCommand(data)) if data.name == "guard" => {
            let sub = data.options.first();
            let value = sub.and_then(|s| match &s.value {
                CommandOptionValue::SubCommand(options) => options.iter().find_map(|o| match &o.value {
                    CommandOptionValue::String(v) => Some(v.clone()),
                    _ => None,
                }),
                _ => None,
            });
            InteractionKind::Command { subcommand: sub.map(|s| s.name.clone()).unwrap_or_default(), value }
        }
        _ => InteractionKind::Other,
    };
    InteractionIn {
        id: interaction.id.to_string(),
        token: interaction.token.clone(),
        guild_id: interaction.guild_id.map(|g| g.to_string()),
        user_id: interaction.author_id().map(|u| u.to_string()).unwrap_or_default(),
        permissions: interaction.member.as_ref().and_then(|m| m.permissions).map(|p| p.bits()).unwrap_or(0),
        kind,
    }
}
