//! The bot end to end against stand-ins for Jev and Discord and a throwaway
//! Postgres: shadow mode reports only, /jev mode enforce acts, a second
//! strike bans, members are never banned, staff are skipped, mods undo from
//! the #mod-log button, and non-mods can't.

use std::sync::{Arc, Mutex};

use axum::{Json, Router, body::Bytes, http::{Method, StatusCode, Uri}, response::IntoResponse, routing::post};
use chrono::{Duration, Utc};
use jev_discord_bot::{
    bot::{Bot, InteractionIn, InteractionKind, MIGRATOR},
    config::Config,
    rules::{Incoming, Thresholds},
};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;

type Calls = Arc<Mutex<Vec<(String, String, String)>>>;

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{address}")
}

/// Jev, answering by what the message says.
async fn jev() -> String {
    async fn evaluate(Json(body): Json<Value>) -> Json<Value> {
        let text = body["state"]["text"].as_str().unwrap_or("").to_lowercase();
        assert!(body["state"]["facts"]["community"].is_string() && body["questions"]["lure"].is_object(), "{body}");
        let (kind, spam, scam, promo, lure) = if text.contains("free nitro") {
            ("scam_or_phishing", 0.03, 0.95, 0.0, 0.97)
        } else if text.contains("buy my course") {
            ("spam", 0.95, 0.01, 0.02, 0.6)
        } else if text.contains("check out my server") {
            ("self_promo_off_topic", 0.2, 0.0, 0.5, 0.4)
        } else {
            ("legit", 0.02, 0.01, 0.02, 0.05)
        };
        Json(json!({
            "model": "stub-1",
            "answers": {
                "kind": { "type": "choice", "choice": kind, "confidence": 0.9,
                          "probabilities": { "legit": 1.0 - spam - scam - promo, "spam": spam, "scam_or_phishing": scam, "self_promo_off_topic": promo } },
                "lure": { "type": "noul", "noul": lure }
            },
            "usage": { "input_tokens": 120, "output_tokens": 8 }
        }))
    }
    format!("{}/", serve(Router::new().route("/", post(evaluate))).await)
}

/// Discord: records every call and says yes.
async fn discord() -> (String, Calls) {
    let calls: Calls = Arc::default();
    let recorded = calls.clone();
    let router = Router::new().fallback(move |method: Method, uri: Uri, body: Bytes| {
        let recorded = recorded.clone();
        async move {
            recorded.lock().unwrap().push((method.to_string(), uri.path().to_owned(), String::from_utf8_lossy(&body).into_owned()));
            match (method, uri.path()) {
                (Method::GET, p) if p.starts_with("/channels/") => Json(json!({ "name": "general" })).into_response(),
                (Method::POST, p) if p.ends_with("/messages") => Json(json!({ "id": "report-1" })).into_response(),
                _ => StatusCode::NO_CONTENT.into_response(),
            }
        }
    });
    (serve(router).await, calls)
}

fn count(calls: &Calls, method: &str, path: &str) -> usize {
    calls.lock().unwrap().iter().filter(|(m, p, _)| m == method && p.contains(path)).count()
}

fn message(id: &str, author: &str, text: &str, roles: &[&str], joined_days: i64) -> Incoming {
    Incoming {
        guild_id: "guild1".into(),
        message_id: id.into(),
        channel_id: "chan".into(),
        author_id: author.into(),
        username: format!("u{author}"),
        content: text.into(),
        author_is_bot: false,
        joined_at: Some(Utc::now() - Duration::days(joined_days)),
        role_ids: roles.iter().map(|r| (*r).to_owned()).collect(),
        mentions_everyone: false,
    }
}

fn press(id: uuid::Uuid, permissions: u64) -> InteractionIn {
    InteractionIn {
        id: "i1".into(),
        token: "tok".into(),
        guild_id: Some("guild1".into()),
        user_id: "mod1".into(),
        permissions,
        kind: InteractionKind::Button { custom_id: format!("jev:undo:{id}"), message_content: "[report]".into() },
    }
}

fn command(subcommand: &str, value: Option<&str>, permissions: u64) -> InteractionIn {
    InteractionIn {
        id: "i2".into(),
        token: "tok".into(),
        guild_id: Some("guild1".into()),
        user_id: "owner1".into(),
        permissions,
        kind: InteractionKind::Command { subcommand: subcommand.into(), value: value.map(str::to_owned) },
    }
}

#[tokio::test]
async fn the_bot_end_to_end() {
    unsafe { std::env::set_var("LC_ALL", "C") };
    let temp = pgtemp::PgTempDB::builder().with_config_param("listen_addresses", "127.0.0.1").start_async().await;
    let pool = PgPoolOptions::new().connect(&temp.connection_uri().replace("localhost", "127.0.0.1")).await.unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    let (discord_base, calls) = discord().await;
    let config = Config {
        bot_token: "bot".into(),
        guild_id: "guild1".into(),
        member_role_ids: vec!["builder".into()],
        staff_role_ids: vec!["mods".into()],
        mod_log_channel_id: Some("modlog".into()),
        typesafe_api_key: "ts_test".into(),
        typesafe_endpoint: Some(jev().await),
        database_url: String::new(),
        discord_api_base: discord_base,
        default_mode: "shadow".into(),
        thresholds: Thresholds { confident: 0.9, flag: 0.6 },
        community: "A test server.".into(),
    };
    let bot = Bot::new(config, pool.clone()).unwrap();

    // Shadow mode: a new visitor's scam is reported with a Wrong-call button; nothing is done.
    let id = bot.handle_message(&message("m1", "500", "FREE NITRO https://nitro.example", &[], 1)).await.unwrap().unwrap();
    assert_eq!(count(&calls, "PUT", "/bans/"), 0);
    assert_eq!(count(&calls, "DELETE", "/messages/m1"), 0);
    let report = calls.lock().unwrap().iter().find(|(m, p, _)| m == "POST" && p == "/channels/modlog/messages").unwrap().2.clone();
    assert!(report.contains("[shadow] would **ban**") && report.contains(&format!("jev:undo:{id}")) && report.contains("Wrong call"), "{report}");

    // Ordinary chat from a visitor is judged, not reported.
    assert_eq!(bot.handle_message(&message("m2", "501", "what do you use for evals?", &[], 1)).await.unwrap(), None);

    // Only server managers switch modes.
    let refused = bot.answer(&command("mode", Some("enforce"), 0)).await.unwrap();
    assert!(refused["data"]["content"].as_str().unwrap().contains("Only server managers"));
    let switched = bot.answer(&command("mode", Some("enforce"), 1 << 5)).await.unwrap();
    assert_eq!(switched["data"]["flags"], 64, "a private reply");
    assert_eq!(bot.mode().await.unwrap(), "enforce");

    // Enforced: the scam is deleted and banned.
    let ban = bot.handle_message(&message("m3", "502", "free nitro for everyone", &[], 2)).await.unwrap().unwrap();
    assert_eq!(count(&calls, "DELETE", "/channels/chan/messages/m3"), 1);
    assert_eq!(count(&calls, "PUT", "/guilds/guild1/bans/502"), 1);

    // An older visitor: kicked, then banned on the second strike.
    bot.handle_message(&message("m4", "503", "buy my course, 90% off", &[], 30)).await.unwrap();
    assert_eq!(count(&calls, "DELETE", "/guilds/guild1/members/503"), 1);
    sqlx::query("UPDATE authors SET last_judged_at=NULL").execute(&pool).await.unwrap();
    bot.handle_message(&message("m5", "503", "buy my course, last chance", &[], 30)).await.unwrap();
    assert_eq!(count(&calls, "PUT", "/bans/503"), 1);

    // A member's scam: deleted and timed out, never kicked or banned.
    bot.handle_message(&message("m6", "504", "free nitro https://x.example", &["builder"], 60)).await.unwrap();
    assert_eq!(count(&calls, "DELETE", "/messages/m6"), 1);
    assert_eq!(count(&calls, "PATCH", "/guilds/guild1/members/504"), 1);
    assert_eq!(count(&calls, "PUT", "/bans/504") + count(&calls, "DELETE", "/guilds/guild1/members/504"), 0);

    // A member's borderline post is only flagged; their ordinary chat never reaches Jev.
    let flagged = bot.handle_message(&message("m7", "505", "check out my server https://mine.example", &["builder"], 60)).await.unwrap();
    assert!(flagged.is_some());
    assert_eq!(count(&calls, "DELETE", "/messages/m7"), 0);
    assert_eq!(bot.handle_message(&message("m8", "506", "free nitro lol jk", &["builder"], 60)).await.unwrap(), None);

    // Mods are never judged.
    assert_eq!(bot.handle_message(&message("m9", "507", "free nitro test", &["mods"], 0)).await.unwrap(), None);

    // A non-mod can't press Undo; a mod can, once.
    let nope = bot.answer(&press(ban, 0)).await.unwrap();
    assert!(nope["data"]["content"].as_str().unwrap().contains("Only mods"));
    assert_eq!(count(&calls, "DELETE", "/guilds/guild1/bans/502"), 0);
    let undone = bot.answer(&press(ban, 1 << 2)).await.unwrap();
    assert_eq!(undone["type"], 7, "the report is updated in place");
    assert!(undone["data"]["content"].as_str().unwrap().contains("unbanned"));
    assert_eq!(undone["data"]["components"], json!([]));
    assert_eq!(count(&calls, "DELETE", "/guilds/guild1/bans/502"), 1);
    let again = bot.answer(&press(ban, 1 << 2)).await.unwrap();
    assert!(again["data"]["content"].as_str().unwrap().contains("Already undone"));

    // Status sums it up.
    let status = bot.answer(&command("status", None, 1 << 3)).await.unwrap();
    let text = status["data"]["content"].as_str().unwrap();
    assert!(text.contains("**enforce**") && text.contains("1 undone"), "{text}");
    drop(temp);
}
