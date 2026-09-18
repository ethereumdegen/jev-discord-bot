//! The Discord REST calls the bot makes. The gateway (in main.rs) only reads.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

#[derive(Clone)]
pub struct Discord {
    http: reqwest::Client,
    base: String,
    token: String,
    guild_id: String,
}

impl Discord {
    pub fn new(http: reqwest::Client, base: &str, token: &str, guild_id: &str) -> Self {
        Self { http, base: base.to_owned(), token: token.to_owned(), guild_id: guild_id.to_owned() }
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<(StatusCode, Value)> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.base))
            .header("Authorization", format!("Bot {}", self.token))
            .header("X-Audit-Log-Reason", "jev-discord-bot");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("could not reach Discord")?;
        let status = response.status();
        let body = response.json::<Value>().await.unwrap_or(Value::Null);
        if status == StatusCode::TOO_MANY_REQUESTS {
            bail!("Discord rate limit; retry after {:.1}s", body["retry_after"].as_f64().unwrap_or(1.0));
        }
        Ok((status, body))
    }

    /// Success, or already gone (a deleted message, a member who left).
    async fn done(&self, method: Method, path: &str, body: Option<Value>, what: &str) -> Result<()> {
        let (status, body) = self.call(method, path, body).await?;
        if status.is_success() || status == StatusCode::NOT_FOUND { Ok(()) } else { Err(anyhow!("Discord refused to {what} ({status}): {body}")) }
    }

    pub async fn channel_name(&self, channel_id: &str) -> Option<String> {
        let (status, body) = self.call(Method::GET, &format!("/channels/{channel_id}"), None).await.ok()?;
        status.is_success().then(|| body["name"].as_str().map(str::to_owned)).flatten()
    }

    pub async fn delete_message(&self, channel_id: &str, message_id: &str) -> Result<()> {
        self.done(Method::DELETE, &format!("/channels/{channel_id}/messages/{message_id}"), None, "delete the message").await
    }

    pub async fn kick(&self, user_id: &str) -> Result<()> {
        self.done(Method::DELETE, &format!("/guilds/{}/members/{user_id}", self.guild_id), None, "kick").await
    }

    /// Ban, deleting their last hour of messages.
    pub async fn ban(&self, user_id: &str) -> Result<()> {
        self.done(Method::PUT, &format!("/guilds/{}/bans/{user_id}", self.guild_id), Some(json!({ "delete_message_seconds": 3600 })), "ban").await
    }

    pub async fn unban(&self, user_id: &str) -> Result<()> {
        self.done(Method::DELETE, &format!("/guilds/{}/bans/{user_id}", self.guild_id), None, "unban").await
    }

    /// Time out until `until`, or lift it with `None`.
    pub async fn timeout(&self, user_id: &str, until: Option<DateTime<Utc>>) -> Result<()> {
        let body = json!({ "communication_disabled_until": until.map(|u| u.to_rfc3339()) });
        self.done(Method::PATCH, &format!("/guilds/{}/members/{user_id}", self.guild_id), Some(body), "time out").await
    }

    /// Post a message (no pings), optionally with components; returns its id.
    pub async fn post(&self, channel_id: &str, content: &str, components: Option<Value>) -> Result<String> {
        let mut body = json!({ "content": content, "allowed_mentions": { "parse": [] } });
        if let Some(components) = components {
            body["components"] = components;
        }
        let (status, answer) = self.call(Method::POST, &format!("/channels/{channel_id}/messages"), Some(body)).await?;
        if !status.is_success() {
            bail!("Discord refused the message ({status}): {answer}");
        }
        Ok(answer["id"].as_str().unwrap_or_default().to_owned())
    }

    /// Answer an interaction (a button press or a slash command).
    pub async fn respond(&self, interaction_id: &str, token: &str, body: Value) -> Result<()> {
        let (status, answer) = self.call(Method::POST, &format!("/interactions/{interaction_id}/{token}/callback"), Some(body)).await?;
        if status.is_success() { Ok(()) } else { Err(anyhow!("Discord refused the interaction response ({status}): {answer}")) }
    }

    /// Replace the guild's slash commands with ours.
    pub async fn register_commands(&self, application_id: &str, commands: Value) -> Result<()> {
        let (status, answer) = self.call(Method::PUT, &format!("/applications/{application_id}/guilds/{}/commands", self.guild_id), Some(commands)).await?;
        if status.is_success() { Ok(()) } else { Err(anyhow!("Discord refused the commands ({status}): {answer}")) }
    }
}
