//! Redis (Upstash in production): everything on the per-message path, so a
//! busy server never waits on Postgres.
//!
//! - `jev:messages`: a stream from the gateway to the workers (consumer group
//!   `workers`), so a spam raid is queued, not dropped;
//! - `jev:guild:{id}`: each server's settings, cached for five minutes;
//! - cooldowns, message dedupe, per-author message counts, and each server's
//!   monthly usage.

use anyhow::{Context, Result};
use redis::{AsyncCommands, aio::ConnectionManager, streams::StreamReadOptions};

pub const STREAM: &str = "jev:messages";
const GROUP: &str = "workers";
/// Cap the stream; old delivered entries are dropped.
const STREAM_MAX: usize = 100_000;

#[derive(Clone)]
pub struct Hot {
    conn: ConnectionManager,
}

impl Hot {
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url).context("REDIS_URL is not a Redis URL")?;
        let conn = ConnectionManager::new(client).await.context("connect to Redis")?;
        let hot = Self { conn };
        hot.ensure_group().await?;
        Ok(hot)
    }

    async fn ensure_group(&self) -> Result<()> {
        let mut conn = self.conn.clone();
        let created: redis::RedisResult<()> = redis::cmd("XGROUP").arg("CREATE").arg(STREAM).arg(GROUP).arg("$").arg("MKSTREAM").query_async(&mut conn).await;
        match created {
            Ok(()) => Ok(()),
            Err(error) if error.to_string().contains("BUSYGROUP") => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Queue a message for the workers, once: a resumed gateway can deliver it twice.
    pub async fn enqueue(&self, message_id: &str, payload: &str) -> Result<bool> {
        let mut conn = self.conn.clone();
        let fresh: bool = redis::cmd("SET").arg(format!("jev:seen:{message_id}")).arg(1).arg("NX").arg("EX").arg(3600).query_async::<Option<String>>(&mut conn).await?.is_some();
        if fresh {
            let _: String = redis::cmd("XADD").arg(STREAM).arg("MAXLEN").arg("~").arg(STREAM_MAX).arg("*").arg("m").arg(payload).query_async(&mut conn).await?;
        }
        Ok(fresh)
    }

    /// Up to `count` queued messages for this worker: first any another worker
    /// left unacknowledged for a minute, then new ones (waiting up to `block_ms`).
    pub async fn next_batch(&self, consumer: &str, count: usize, block_ms: usize) -> Result<Vec<(String, String)>> {
        let mut conn = self.conn.clone();
        let claimed: redis::Value = redis::cmd("XAUTOCLAIM").arg(STREAM).arg(GROUP).arg(consumer).arg(60_000).arg("0-0").arg("COUNT").arg(count).query_async(&mut conn).await?;
        let mut out = entries_from_autoclaim(claimed);
        if out.is_empty() {
            let options = StreamReadOptions::default().group(GROUP, consumer).count(count).block(block_ms);
            let reply: redis::streams::StreamReadReply = conn.xread_options(&[STREAM], &[">"], &options).await?;
            for key in reply.keys {
                for entry in key.ids {
                    if let Some(redis::Value::BulkString(bytes)) = entry.map.get("m") {
                        out.push((entry.id.clone(), String::from_utf8_lossy(bytes).into_owned()));
                    }
                }
            }
        }
        Ok(out)
    }

    pub async fn ack(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: i64 = conn.xack(STREAM, GROUP, &[id]).await?;
        let _: i64 = conn.xdel(STREAM, &[id]).await?;
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        let mut conn = self.conn.clone();
        Ok(conn.get(key).await?)
    }

    pub async fn set_ex(&self, key: &str, value: &str, seconds: u64) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = conn.set_ex(key, value, seconds).await?;
        Ok(())
    }

    pub async fn del(&self, key: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: i64 = conn.del(key).await?;
        Ok(())
    }

    /// True the first time in `seconds` for this key: a cooldown.
    pub async fn first_in(&self, key: &str, seconds: u64) -> Result<bool> {
        let mut conn = self.conn.clone();
        let set: Option<String> = redis::cmd("SET").arg(key).arg(1).arg("NX").arg("EX").arg(seconds).query_async(&mut conn).await?;
        Ok(set.is_some())
    }

    /// Increment a counter, giving it an expiry when it's new.
    pub async fn incr(&self, key: &str, expire_seconds: i64) -> Result<i64> {
        let mut conn = self.conn.clone();
        let value: i64 = conn.incr(key, 1).await?;
        if value == 1 {
            let _: bool = conn.expire(key, expire_seconds).await?;
        }
        Ok(value)
    }

    pub async fn counter(&self, key: &str) -> Result<i64> {
        Ok(self.get(key).await?.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub async fn ping(&self) -> bool {
        let mut conn = self.conn.clone();
        redis::cmd("PING").query_async::<String>(&mut conn).await.is_ok()
    }
}

/// XAUTOCLAIM answers `[next-id, [[id, [field, value, ...]], ...], [deleted...]]`.
fn entries_from_autoclaim(value: redis::Value) -> Vec<(String, String)> {
    let redis::Value::Array(parts) = value else { return Vec::new() };
    let Some(redis::Value::Array(entries)) = parts.into_iter().nth(1) else { return Vec::new() };
    entries
        .into_iter()
        .filter_map(|entry| {
            let redis::Value::Array(mut pair) = entry else { return None };
            let fields = pair.pop()?;
            let id = match pair.pop()? {
                redis::Value::BulkString(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                _ => return None,
            };
            let redis::Value::Array(fields) = fields else { return None };
            let mut iter = fields.into_iter();
            while let (Some(name), Some(value)) = (iter.next(), iter.next()) {
                if let (redis::Value::BulkString(name), redis::Value::BulkString(value)) = (name, value)
                    && name == b"m"
                {
                    return Some((id, String::from_utf8_lossy(&value).into_owned()));
                }
            }
            None
        })
        .collect()
}

/// The month key for usage counters: `2026-09`.
pub fn month(now: chrono::DateTime<chrono::Utc>) -> String {
    now.format("%Y-%m").to_string()
}

pub fn usage_key(guild_id: &str, month: &str) -> String {
    format!("jev:usage:{guild_id}:{month}")
}
