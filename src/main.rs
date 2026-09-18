//! One gateway shard: messages go to the bot, button presses and /jev commands
//! to its interaction handler. Everything else lives in the library.

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use jev_discord_bot::{
    bot::{self, Bot, InteractionIn, InteractionKind},
    config::Config,
    rules::Incoming,
};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};
use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_model::{
    application::interaction::{Interaction, InteractionData, application_command::CommandOptionValue},
    channel::Message,
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("jev_discord_bot=info")))
        .with(fmt::layer().json())
        .init();
    // The gateway's TLS needs a process-wide crypto provider.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = Config::from_env()?;
    let pool = bot::connect(&config.database_url).await?;
    let token = config.bot_token.clone();
    let bot = Arc::new(Bot::new(config, pool)?);
    tracing::info!(mode = %bot.mode().await?, "connecting to the gateway");

    let intents = Intents::GUILDS | Intents::GUILD_MESSAGES | Intents::MESSAGE_CONTENT;
    let mut shard = Shard::new(ShardId::ONE, token, intents);
    let wanted = EventTypeFlags::READY | EventTypeFlags::MESSAGE_CREATE | EventTypeFlags::INTERACTION_CREATE;
    while let Some(item) = shard.next_event(wanted).await {
        let event = match item {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!(?error, "gateway error");
                continue;
            }
        };
        let bot = bot.clone();
        match event {
            Event::Ready(ready) => {
                tracing::info!(user = %ready.user.name, "ready");
                let application = ready.application.id.to_string();
                tokio::spawn(async move {
                    if let Err(error) = bot.discord.register_commands(&application, bot::commands()).await {
                        tracing::warn!(?error, "could not register /jev");
                    }
                });
            }
            Event::MessageCreate(message) => {
                let incoming = incoming(&message.0);
                tokio::spawn(async move {
                    if let Err(error) = bot.handle_message(&incoming).await {
                        tracing::warn!(error = %format!("{error:#}"), message_id = %incoming.message_id, "could not moderate a message");
                    }
                });
            }
            Event::InteractionCreate(interaction) => {
                let interaction = interaction_in(&interaction.0);
                tokio::spawn(async move {
                    if let Err(error) = bot.handle_interaction(&interaction).await {
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
        Some(InteractionData::ApplicationCommand(data)) if data.name == "jev" => {
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
