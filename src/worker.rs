//! Workers pop queued messages and run the engine on each. Several can run
//! at once (each with its own consumer name); a message one of them left
//! unacknowledged for a minute is picked up by another.

use anyhow::Result;

use crate::{engine, rules::Incoming, state::AppState};

/// Messages judged in parallel per worker.
const BATCH: usize = 16;

pub async fn run(state: AppState, consumer: String) -> Result<()> {
    tracing::info!(%consumer, "worker started");
    loop {
        let batch = match state.hot.next_batch(&consumer, BATCH, 5_000).await {
            Ok(batch) => batch,
            Err(error) => {
                tracing::warn!(?error, "could not read the queue");
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };
        let tasks: Vec<_> = batch.into_iter().map(|(id, payload)| {
            let state = state.clone();
            tokio::spawn(async move { process(&state, &id, &payload).await })
        }).collect();
        for task in tasks {
            let _ = task.await;
        }
    }
}

/// One queued message. Acknowledged unless it failed in a way worth retrying
/// (Jev or Discord unreachable), in which case another pass picks it up.
pub async fn process(state: &AppState, id: &str, payload: &str) -> Result<()> {
    let Ok(incoming) = serde_json::from_str::<Incoming>(payload) else {
        return state.hot.ack(id).await;
    };
    match engine::handle_message(state, &incoming).await {
        Ok(_) => state.hot.ack(id).await,
        Err(error) => {
            // Give up after five tries so one bad message can't circle forever.
            let tries = state.hot.incr(&format!("jev:tries:{}", incoming.message_id), 3600).await.unwrap_or(99);
            tracing::warn!(error = %format!("{error:#}"), message_id = %incoming.message_id, tries, "could not moderate a message");
            if tries >= 5 { state.hot.ack(id).await } else { Ok(()) }
        }
    }
}
