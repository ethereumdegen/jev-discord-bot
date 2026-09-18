//! `jevmod <role>`: web (the site and API), gateway (the Discord connection),
//! worker (judging), migrate, or all three services in one process.

use anyhow::{Context, Result, bail};
use jev_discord_bot::{
    config::Config,
    gateway, hot::Hot, http,
    state::{self, AppState, MIGRATOR},
    worker,
};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("jev_discord_bot=info,tower_http=info")))
        .with(fmt::layer().json())
        .init();
    let role = std::env::args().nth(1).or_else(|| std::env::var("ROLE").ok()).unwrap_or_else(|| "all".into());
    let config = Config::from_env()?;
    let pool = state::connect(&config.database_url).await.context("connect to Postgres")?;
    if role == "migrate" {
        return Ok(MIGRATOR.run(&pool).await?);
    }
    if role == "all" {
        // Local runs have no separate migrate step.
        MIGRATOR.run(&pool).await?;
    }
    let hot = Hot::connect(&config.redis_url).await?;
    let bind = config.bind;
    let state = AppState::new(config, pool, hot)?;
    let consumer = std::env::var("RAILWAY_REPLICA_ID").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| format!("worker-{}", std::process::id()));
    match role.as_str() {
        "web" => serve(state, bind).await,
        "gateway" => gateway::run(state).await,
        "worker" => worker::run(state, consumer).await,
        "all" => {
            tokio::spawn(gateway::run(state.clone()));
            tokio::spawn(worker::run(state.clone(), consumer));
            serve(state, bind).await
        }
        // Web and worker without the gateway: for running against stand-ins.
        "local" => {
            tokio::spawn(worker::run(state.clone(), consumer));
            serve(state, bind).await
        }
        other => bail!("unknown role {other}: use web, gateway, worker, migrate, all or local"),
    }
}

async fn serve(state: AppState, bind: std::net::SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "web listening");
    axum::serve(listener, http::app(state)).with_graceful_shutdown(async { let _ = tokio::signal::ctrl_c().await; }).await?;
    Ok(())
}
