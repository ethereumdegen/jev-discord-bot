use std::env;

use anyhow::{Context, Result, bail};

use crate::rules::Thresholds;

#[derive(Clone)]
pub struct Config {
    pub bot_token: String,
    pub guild_id: String,
    /// Roles whose holders are paying members: never kicked or banned automatically.
    pub member_role_ids: Vec<String>,
    /// Roles the bot never judges (mods, team).
    pub staff_role_ids: Vec<String>,
    /// A staff-only channel the bot reports to, with Undo buttons.
    pub mod_log_channel_id: Option<String>,
    pub typesafe_api_key: String,
    /// Jev's endpoint; a stand-in in tests.
    pub typesafe_endpoint: Option<String>,
    pub database_url: String,
    /// `https://discord.com/api/v10`; a stand-in in tests.
    pub discord_api_base: String,
    /// The mode until a mod sets one with /jev: shadow (log only) or enforce.
    pub default_mode: String,
    pub thresholds: Thresholds,
    /// Plain-language description of the server, given to Jev with every message.
    pub community: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("guild_id", &self.guild_id)
            .field("member_role_ids", &self.member_role_ids)
            .field("staff_role_ids", &self.staff_role_ids)
            .field("mod_log_channel_id", &self.mod_log_channel_id)
            .field("default_mode", &self.default_mode)
            .finish_non_exhaustive()
    }
}

fn list(value: Option<String>) -> Vec<String> {
    value.map(|v| v.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

fn percent(value: Option<String>, name: &str, default: f64) -> Result<f64> {
    match value {
        None => Ok(default),
        Some(v) => {
            let n: f64 = v.parse().with_context(|| format!("{name} must be a number from 1 to 100"))?;
            if !(1.0..=100.0).contains(&n) {
                bail!("{name} must be from 1 to 100");
            }
            Ok(n / 100.0)
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let get = |name: &str| env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let need = |name: &str| get(name).with_context(|| format!("{name} is required"));
        let default_mode = get("DEFAULT_MODE").unwrap_or_else(|| "shadow".into());
        if !matches!(default_mode.as_str(), "shadow" | "enforce") {
            bail!("DEFAULT_MODE is shadow or enforce");
        }
        let thresholds = Thresholds {
            confident: percent(get("CONFIDENT_PERCENT"), "CONFIDENT_PERCENT", 0.9)?,
            flag: percent(get("FLAG_PERCENT"), "FLAG_PERCENT", 0.6)?,
        };
        if thresholds.flag > thresholds.confident {
            bail!("FLAG_PERCENT can't be above CONFIDENT_PERCENT");
        }
        Ok(Self {
            bot_token: need("DISCORD_BOT_TOKEN")?,
            guild_id: need("DISCORD_GUILD_ID")?,
            member_role_ids: list(get("MEMBER_ROLE_IDS")),
            staff_role_ids: list(get("STAFF_ROLE_IDS")),
            mod_log_channel_id: get("MOD_LOG_CHANNEL_ID"),
            typesafe_api_key: need("TYPESAFE_API_KEY")?,
            typesafe_endpoint: get("TYPESAFE_ENDPOINT"),
            database_url: need("DATABASE_URL")?,
            discord_api_base: get("DISCORD_API_BASE").unwrap_or_else(|| "https://discord.com/api/v10".into()).trim_end_matches('/').to_owned(),
            default_mode,
            thresholds,
            community: get("COMMUNITY_DESCRIPTION").unwrap_or_else(|| {
                "A Discord for AI developers and agentic coders. Talking shop, sharing your own projects in the right channel, and asking for help are all normal.".into()
            }),
        })
    }
}
