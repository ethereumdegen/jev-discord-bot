use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, bail};

/// The Discord application: the bot token, and OAuth for connecting accounts
/// and installing the bot.
#[derive(Clone)]
pub struct DiscordConfig {
    pub client_id: String,
    pub client_secret: String,
    pub bot_token: String,
    /// `https://discord.com/api/v10`; a stand-in in tests.
    pub api_base: String,
    /// Where browsers go to authorize; `https://discord.com`.
    pub authorize_base: String,
}

#[derive(Clone)]
pub struct GoogleConfig {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Clone)]
pub struct Config {
    /// What the product is called on the site and in Discord.
    pub brand: String,
    /// `https://jevmod.example`, without a trailing slash.
    pub base_url: String,
    pub bind: SocketAddr,
    pub production: bool,
    pub database_url: String,
    /// Upstash (`rediss://…`) or a local Redis.
    pub redis_url: String,
    pub static_dir: PathBuf,
    pub discord: DiscordConfig,
    pub google: Option<GoogleConfig>,
    pub typesafe_api_key: String,
    /// Jev's endpoint; a stand-in in tests.
    pub typesafe_endpoint: Option<String>,
    /// Emails that become operators (see every server, set allowances) when they sign in.
    pub operator_emails: Vec<String>,
    pub default_allowance: i32,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config").field("brand", &self.brand).field("base_url", &self.base_url).finish_non_exhaustive()
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let get = |name: &str| env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let need = |name: &str| get(name).with_context(|| format!("{name} is required"));
        let port: u16 = get("PORT").unwrap_or_else(|| "3120".into()).parse().context("PORT must be a port number")?;
        let base_url = get("APP_BASE_URL").unwrap_or_else(|| format!("http://localhost:{port}")).trim_end_matches('/').to_owned();
        let production = get("APP_ENV").as_deref() == Some("production");
        if production && base_url.starts_with("http://") {
            bail!("APP_BASE_URL must be https in production");
        }
        let google = match (get("GOOGLE_CLIENT_ID"), get("GOOGLE_CLIENT_SECRET")) {
            (Some(client_id), Some(client_secret)) => Some(GoogleConfig { client_id, client_secret }),
            (None, None) => None,
            _ => bail!("set GOOGLE_CLIENT_ID and GOOGLE_CLIENT_SECRET together"),
        };
        Ok(Self {
            brand: get("BRAND").unwrap_or_else(|| "Degen Guard".into()),
            bind: SocketAddr::from(([0, 0, 0, 0], port)),
            production,
            database_url: need("DATABASE_URL")?,
            redis_url: need("REDIS_URL")?,
            static_dir: get("STATIC_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("frontend/dist")),
            discord: DiscordConfig {
                client_id: need("DISCORD_CLIENT_ID")?,
                client_secret: need("DISCORD_CLIENT_SECRET")?,
                bot_token: need("DISCORD_BOT_TOKEN")?,
                api_base: get("DISCORD_API_BASE").unwrap_or_else(|| "https://discord.com/api/v10".into()).trim_end_matches('/').to_owned(),
                authorize_base: get("DISCORD_AUTHORIZE_BASE").unwrap_or_else(|| "https://discord.com".into()).trim_end_matches('/').to_owned(),
            },
            google,
            typesafe_api_key: need("TYPESAFE_API_KEY")?,
            typesafe_endpoint: get("TYPESAFE_ENDPOINT"),
            operator_emails: get("OPERATOR_EMAILS").map(|v| v.split(',').map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()).collect()).unwrap_or_default(),
            default_allowance: get("DEFAULT_MONTHLY_ALLOWANCE").map(|v| v.parse()).transpose().context("DEFAULT_MONTHLY_ALLOWANCE must be a number")?.unwrap_or(10_000),
            base_url,
        })
    }
}
