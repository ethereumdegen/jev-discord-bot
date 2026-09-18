//! A server's settings and its spam rule. Workers read them through a
//! five-minute Redis cache; the website clears it on every save.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::{
    hot::Hot,
    rules::{Step, Thresholds, parse_ladder},
};

const CACHE_SECONDS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Guild {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub owner_user_id: Option<String>,
    pub removed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub mode: String,
    pub log_channel_id: Option<String>,
    pub exempt_role_ids: Vec<String>,
    pub exempt_channel_ids: Vec<String>,
    pub confident_percent: i32,
    pub flag_percent: i32,
    pub strike_days: i32,
    pub timeout_minutes: i32,
    pub community: String,
    pub monthly_allowance: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Rule {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    pub ladder: Vec<String>,
}

/// What a worker needs for one server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub guild: Guild,
    pub spam: Rule,
}

impl Settings {
    pub fn thresholds(&self) -> Thresholds {
        Thresholds { confident: f64::from(self.guild.confident_percent) / 100.0, flag: f64::from(self.guild.flag_percent) / 100.0 }
    }

    pub fn ladder(&self) -> Vec<Step> {
        parse_ladder(&self.spam.ladder).unwrap_or_else(|| vec![Step::Warn, Step::Kick, Step::Ban])
    }
}

pub const GUILD_COLUMNS: &str = "id,name,icon,owner_user_id,removed_at,mode,log_channel_id,exempt_role_ids,exempt_channel_ids,confident_percent,flag_percent,strike_days,timeout_minutes,community,monthly_allowance";

fn cache_key(guild_id: &str) -> String {
    format!("jev:guild:{guild_id}")
}

pub async fn load_fresh(pool: &PgPool, guild_id: &str) -> Result<Option<Settings>> {
    let Some(guild) = sqlx::query_as::<_, Guild>(&format!("SELECT {GUILD_COLUMNS} FROM guilds WHERE id=$1")).bind(guild_id).fetch_optional(pool).await? else {
        return Ok(None);
    };
    let spam = sqlx::query_as::<_, Rule>("SELECT id,kind,name,enabled,ladder FROM rules WHERE guild_id=$1 AND kind='spam'").bind(guild_id).fetch_one(pool).await?;
    Ok(Some(Settings { guild, spam }))
}

/// Settings through the cache.
pub async fn load(pool: &PgPool, hot: &Hot, guild_id: &str) -> Result<Option<Settings>> {
    if let Some(cached) = hot.get(&cache_key(guild_id)).await?
        && let Ok(settings) = serde_json::from_str(&cached)
    {
        return Ok(Some(settings));
    }
    let settings = load_fresh(pool, guild_id).await?;
    if let Some(settings) = &settings {
        hot.set_ex(&cache_key(guild_id), &serde_json::to_string(settings)?, CACHE_SECONDS).await?;
    }
    Ok(settings)
}

pub async fn forget(hot: &Hot, guild_id: &str) -> Result<()> {
    hot.del(&cache_key(guild_id)).await
}

/// The bot is in this server (it just joined, or the gateway reconnected):
/// make sure there's a row and a spam rule. New servers start in watch mode.
pub async fn installed(pool: &PgPool, hot: &Hot, id: &str, name: &str, icon: Option<&str>, owner_user_id: Option<&str>, default_allowance: i32) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO guilds(id,name,icon,owner_user_id,monthly_allowance) VALUES($1,$2,$3,$4,$5)
         ON CONFLICT(id) DO UPDATE SET name=excluded.name,icon=excluded.icon,owner_user_id=COALESCE(excluded.owner_user_id,guilds.owner_user_id),
           removed_at=NULL,installed_at=CASE WHEN guilds.removed_at IS NULL THEN guilds.installed_at ELSE now() END,updated_at=now()",
    )
    .bind(id)
    .bind(name)
    .bind(icon)
    .bind(owner_user_id)
    .bind(default_allowance)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO rules(id,guild_id,kind,name) VALUES($1,$2,'spam','No spam') ON CONFLICT DO NOTHING").bind(Uuid::new_v4()).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    forget(hot, id).await
}

/// The bot was removed from this server. Its data stays for a week in case it comes back.
pub async fn removed(pool: &PgPool, hot: &Hot, id: &str) -> Result<()> {
    sqlx::query("UPDATE guilds SET removed_at=now(),updated_at=now() WHERE id=$1 AND removed_at IS NULL").bind(id).execute(pool).await?;
    forget(hot, id).await
}
