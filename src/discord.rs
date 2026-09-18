//! Discord over REST: the bot's actions in any server it's in, and the OAuth
//! calls for connecting an account and installing the bot. The gateway (in
//! gateway.rs) only reads.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::DiscordConfig;

pub const PERM_ADMINISTRATOR: u64 = 1 << 3;
pub const PERM_MANAGE_GUILD: u64 = 1 << 5;
pub const PERM_BAN_MEMBERS: u64 = 1 << 2;
pub const PERM_MODERATE_MEMBERS: u64 = 1 << 40;
/// What the bot asks for when it's added: View Channels, Send Messages, Manage
/// Messages, Read Message History, Kick, Ban, Moderate Members.
pub const BOT_PERMISSIONS: u64 = (1 << 10) | (1 << 11) | (1 << 13) | (1 << 16) | (1 << 1) | (1 << 2) | (1 << 40);

#[derive(Clone)]
pub struct Discord {
    http: reqwest::Client,
    config: DiscordConfig,
}

/// What an action attempt came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Done,
    /// Discord refused: missing permission, or the target outranks the bot.
    Refused(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub global_name: Option<String>,
    pub email: Option<String>,
    pub verified: Option<bool>,
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PartialGuild {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    #[serde(default)]
    pub owner: bool,
    /// Bitfield as a string.
    #[serde(default)]
    pub permissions: String,
}

impl PartialGuild {
    pub fn manageable(&self) -> bool {
        let perms: u64 = self.permissions.parse().unwrap_or(0);
        self.owner || perms & (PERM_ADMINISTRATOR | PERM_MANAGE_GUILD) != 0
    }
}

/// The OAuth token answer; `guild` is there when the bot was just installed.
#[derive(Debug, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub guild: Option<Value>,
}

impl Discord {
    pub fn new(http: reqwest::Client, config: DiscordConfig) -> Self {
        Self { http, config }
    }

    pub fn config(&self) -> &DiscordConfig {
        &self.config
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<(StatusCode, Value)> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.config.api_base))
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .header("X-Audit-Log-Reason", "Degen Guard");
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

    /// An action: done (or already gone), refused (403), or an error to retry.
    async fn act(&self, method: Method, path: &str, body: Option<Value>) -> Result<Outcome> {
        let (status, body) = self.call(method, path, body).await?;
        match status {
            s if s.is_success() || s == StatusCode::NOT_FOUND => Ok(Outcome::Done),
            StatusCode::FORBIDDEN => Ok(Outcome::Refused(body["message"].as_str().unwrap_or("Missing Permissions").to_owned())),
            _ => Err(anyhow!("Discord answered {status}: {body}")),
        }
    }

    pub async fn channel_name(&self, channel_id: &str) -> Option<String> {
        let (status, body) = self.call(Method::GET, &format!("/channels/{channel_id}"), None).await.ok()?;
        status.is_success().then(|| body["name"].as_str().map(str::to_owned)).flatten()
    }

    pub async fn delete_message(&self, channel_id: &str, message_id: &str) -> Result<Outcome> {
        self.act(Method::DELETE, &format!("/channels/{channel_id}/messages/{message_id}"), None).await
    }

    pub async fn kick(&self, guild_id: &str, user_id: &str) -> Result<Outcome> {
        self.act(Method::DELETE, &format!("/guilds/{guild_id}/members/{user_id}"), None).await
    }

    /// Ban, deleting their last hour of messages.
    pub async fn ban(&self, guild_id: &str, user_id: &str) -> Result<Outcome> {
        self.act(Method::PUT, &format!("/guilds/{guild_id}/bans/{user_id}"), Some(json!({ "delete_message_seconds": 3600 }))).await
    }

    pub async fn unban(&self, guild_id: &str, user_id: &str) -> Result<Outcome> {
        self.act(Method::DELETE, &format!("/guilds/{guild_id}/bans/{user_id}"), None).await
    }

    /// Time out until `until`, or lift it with `None`.
    pub async fn timeout(&self, guild_id: &str, user_id: &str, until: Option<DateTime<Utc>>) -> Result<Outcome> {
        let body = json!({ "communication_disabled_until": until.map(|u| u.to_rfc3339()) });
        self.act(Method::PATCH, &format!("/guilds/{guild_id}/members/{user_id}"), Some(body)).await
    }

    /// Post a message (no pings unless `ping_user`), optionally with components; returns its id.
    pub async fn post(&self, channel_id: &str, content: &str, components: Option<Value>, ping_user: Option<&str>) -> Result<String> {
        let mentions = match ping_user {
            Some(user) => json!({ "parse": [], "users": [user] }),
            None => json!({ "parse": [] }),
        };
        let mut body = json!({ "content": content, "allowed_mentions": mentions });
        if let Some(components) = components {
            body["components"] = components;
        }
        let (status, answer) = self.call(Method::POST, &format!("/channels/{channel_id}/messages"), Some(body)).await?;
        if !status.is_success() {
            bail!("Discord refused the message ({status}): {answer}");
        }
        Ok(answer["id"].as_str().unwrap_or_default().to_owned())
    }

    pub async fn respond(&self, interaction_id: &str, token: &str, body: Value) -> Result<()> {
        let (status, answer) = self.call(Method::POST, &format!("/interactions/{interaction_id}/{token}/callback"), Some(body)).await?;
        if status.is_success() { Ok(()) } else { Err(anyhow!("Discord refused the interaction response ({status}): {answer}")) }
    }

    /// The bot's slash commands, for every server at once.
    pub async fn register_commands(&self, commands: Value) -> Result<()> {
        let (status, answer) = self.call(Method::PUT, &format!("/applications/{}/commands", self.config.client_id), Some(commands)).await?;
        if status.is_success() { Ok(()) } else { Err(anyhow!("Discord refused the commands ({status}): {answer}")) }
    }

    /// Text channels, for choosing a log channel and exempt channels: `[(id, name)]`.
    pub async fn text_channels(&self, guild_id: &str) -> Result<Vec<(String, String)>> {
        let (status, body) = self.call(Method::GET, &format!("/guilds/{guild_id}/channels"), None).await?;
        if !status.is_success() {
            bail!("Discord answered {status} listing channels");
        }
        let mut out: Vec<(i64, String, String)> = body
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|c| matches!(c["type"].as_i64(), Some(0 | 5)))
            .map(|c| (c["position"].as_i64().unwrap_or(0), c["id"].as_str().unwrap_or_default().to_owned(), c["name"].as_str().unwrap_or_default().to_owned()))
            .collect();
        out.sort();
        Ok(out.into_iter().map(|(_, id, name)| (id, name)).collect())
    }

    /// Roles except @everyone and bots' own: `[(id, name)]`.
    pub async fn roles(&self, guild_id: &str) -> Result<Vec<(String, String)>> {
        let (status, body) = self.call(Method::GET, &format!("/guilds/{guild_id}/roles"), None).await?;
        if !status.is_success() {
            bail!("Discord answered {status} listing roles");
        }
        let mut out: Vec<(i64, String, String)> = body
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|r| r["id"].as_str() != Some(guild_id) && r["managed"].as_bool() != Some(true))
            .map(|r| (-r["position"].as_i64().unwrap_or(0), r["id"].as_str().unwrap_or_default().to_owned(), r["name"].as_str().unwrap_or_default().to_owned()))
            .collect();
        out.sort();
        Ok(out.into_iter().map(|(_, id, name)| (id, name)).collect())
    }

    // OAuth ------------------------------------------------------------------------

    pub fn authorize_url(&self, redirect_uri: &str, state: &str, install: bool) -> String {
        let mut url = url::Url::parse(&format!("{}/oauth2/authorize", self.config.authorize_base)).expect("a valid base");
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("client_id", &self.config.client_id)
                .append_pair("redirect_uri", redirect_uri)
                .append_pair("response_type", "code")
                .append_pair("state", state);
            if install {
                q.append_pair("scope", "bot applications.commands identify guilds").append_pair("permissions", &BOT_PERMISSIONS.to_string());
            } else {
                q.append_pair("scope", "identify email guilds").append_pair("prompt", "none");
            }
        }
        url.into()
    }

    pub async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<Token> {
        let response = self
            .http
            .post(format!("{}/oauth2/token", self.config.api_base))
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.as_str()),
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ])
            .send()
            .await
            .context("could not reach Discord")?;
        if !response.status().is_success() {
            bail!("Discord refused the code ({})", response.status());
        }
        Ok(response.json().await?)
    }

    pub async fn me(&self, access_token: &str) -> Result<User> {
        let response = self.http.get(format!("{}/users/@me", self.config.api_base)).bearer_auth(access_token).send().await?;
        if !response.status().is_success() {
            bail!("Discord /users/@me answered {}", response.status());
        }
        Ok(response.json().await?)
    }

    pub async fn my_guilds(&self, access_token: &str) -> Result<Vec<PartialGuild>> {
        let response = self.http.get(format!("{}/users/@me/guilds", self.config.api_base)).bearer_auth(access_token).send().await?;
        if !response.status().is_success() {
            bail!("Discord /users/@me/guilds answered {}", response.status());
        }
        Ok(response.json().await?)
    }
}
