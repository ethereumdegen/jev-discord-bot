use std::sync::Arc;

use anyhow::Result;
use jev::AsyncTypeSafeClient;
use sqlx::PgPool;

use crate::{config::Config, discord::Discord, hot::Hot};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub hot: Hot,
    pub discord: Discord,
    pub jev: Arc<AsyncTypeSafeClient>,
    pub http: reqwest::Client,
}

impl AppState {
    pub fn new(config: Config, pool: PgPool, hot: Hot) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("degen-guard/0.2")
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(20))
            .build()?;
        let discord = Discord::new(http.clone(), config.discord.clone());
        let mut jev = AsyncTypeSafeClient::from_key(&config.typesafe_api_key).map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Some(endpoint) = &config.typesafe_endpoint {
            jev = jev.with_endpoint(endpoint.clone());
        }
        Ok(Self { config: Arc::new(config), pool, hot, discord, jev: Arc::new(jev), http })
    }
}

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> Result<PgPool> {
    Ok(sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .idle_timeout(std::time::Duration::from_secs(120))
        .connect(url)
        .await?)
}
