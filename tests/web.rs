//! The website: signing in with Discord, adding the bot, who may manage what,
//! settings and the ladder, the audit log and its CSV, and the operator.

mod common;

use axum::http::StatusCode;
use common::*;
use jev_discord_bot::engine;
use serde_json::json;

const MANAGE: &str = "32";

#[tokio::test]
async fn an_owner_adds_the_bot_configures_it_and_reads_the_log() {
    let (w, stubs) = world().await;
    let s = &w.state;
    let alice = Browser::new(s);
    assert_eq!(alice.get("/api/servers").await.status, StatusCode::UNAUTHORIZED);

    // Alice signs in with Discord; she manages g1 and is only a member of g2.
    discord_user(&stubs, "111", "alice@example.com", json!([
        { "id": "g1", "name": "Builders", "icon": null, "owner": false, "permissions": MANAGE },
        { "id": "g2", "name": "Elsewhere", "icon": null, "owner": false, "permissions": "0" }
    ]));
    let signed_in = alice.discord("sign_in", "code-a").await;
    assert_eq!(signed_in.location(), "/servers");
    let me = alice.get("/api/me").await.json();
    assert_eq!(me["account"]["discord_username"], "user111");
    let list = alice.get("/api/servers").await.json();
    assert_eq!(list["servers"].as_array().unwrap().len(), 1, "only servers she can manage");
    assert_eq!(list["servers"][0]["installed"], false);

    // Add to Discord: the install round trip puts the bot in g1 and lands on its page.
    let started = alice.get("/api/auth/discord/start?purpose=install").await;
    let url = started.location();
    assert!(url.contains("scope=bot+applications.commands") && url.contains("permissions="), "{url}");
    let installed = alice.discord("install", "install-1").await;
    assert_eq!(installed.location(), "/servers/g1?installed=1");
    let page = alice.get("/api/servers/g1").await.json();
    assert_eq!(page["server"]["mode"], "watch");
    assert_eq!(page["rules"][0]["ladder"], json!(["warn", "kick", "ban"]));
    assert_eq!(page["month"]["allowance"], 10_000);

    // Channels and roles for the pickers (text channels only; no @everyone or bot roles).
    let picks = alice.get("/api/servers/g1/discord").await.json();
    assert_eq!(picks["channels"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect::<Vec<_>>(), vec!["general", "mod-log"]);
    assert_eq!(picks["roles"].as_array().unwrap().len(), 1);

    // Settings: validated, then saved.
    assert_eq!(alice.patch("/api/servers/g1", json!({ "flag_percent": 95 })).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(alice.patch("/api/servers/g1", json!({ "mode": "nuke" })).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    let saved = alice.patch("/api/servers/g1", json!({ "mode": "enforce", "log_channel_id": "c2", "exempt_role_ids": ["r-mods"], "community": "AI builders." })).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text());
    assert_eq!(saved.json()["server"]["exempt_role_ids"], json!(["r-mods"]));
    let rule = page["rules"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(alice.patch(&format!("/api/servers/g1/rules/{rule}"), json!({ "ladder": ["warn", "ban"] })).await.status, StatusCode::NO_CONTENT);
    assert_eq!(alice.patch(&format!("/api/servers/g1/rules/{rule}"), json!({ "ladder": ["warn", "explode"] })).await.status, StatusCode::UNPROCESSABLE_ENTITY);

    // Messages come in; the settings above are what the engine uses (cache cleared on save).
    let warned = engine::handle_message(s, &message("g1", "m1", "500", "buy my course 90% off", &[], 1)).await.unwrap().unwrap();
    let banned = engine::handle_message(s, &message("g1", "m2", "500", "buy my course now", &[], 1)).await.unwrap().unwrap();
    assert_eq!(w.outcome(banned).await.0, "ban", "the two-step ladder");
    engine::handle_message(s, &message("g1", "m3", "501", "check out my server https://x.example", &[], 1)).await.unwrap().unwrap();

    // The audit log, with filters and paging.
    let all = alice.get("/api/servers/g1/actions").await.json();
    assert_eq!(all["actions"].as_array().unwrap().len(), 3);
    let review = alice.get("/api/servers/g1/actions?filter=review").await.json();
    assert_eq!(review["actions"].as_array().unwrap().len(), 1);
    assert_eq!(alice.get("/api/servers/g1/actions?filter=ban").await.json()["actions"][0]["username"], "u500");
    assert_eq!(alice.get("/api/servers/g1/actions?user=500").await.json()["actions"].as_array().unwrap().len(), 2);
    assert_eq!(alice.get("/api/servers/g1/actions?limit=1").await.json()["actions"].as_array().unwrap().len(), 1);
    let history = alice.get("/api/servers/g1/users/500").await.json();
    assert_eq!(history["live_strikes"], 2);

    // Undo from the web; take action on the review entry.
    let undone = alice.post(&format!("/api/servers/g1/actions/{banned}/undo"), json!({})).await;
    assert_eq!(undone.status, StatusCode::OK, "{}", undone.text());
    assert!(undone.json()["did"].as_str().unwrap().contains("unbanned"));
    assert_eq!(alice.post(&format!("/api/servers/g1/actions/{banned}/undo"), json!({})).await.status, StatusCode::CONFLICT);
    let review_id = review["actions"][0]["id"].as_str().unwrap().to_owned();
    assert!(alice.post(&format!("/api/servers/g1/actions/{review_id}/confirm"), json!({})).await.json()["did"].as_str().unwrap().starts_with("warn"));
    assert_eq!(alice.get("/api/servers/g1/users/500").await.json()["live_strikes"], 1);

    // CSV, with formulas defused.
    engine::handle_message(s, &message("g1", "m9", "502", "=HYPERLINK(\"free nitro\")", &[], 1)).await.unwrap();
    let csv = alice.get("/api/servers/g1/actions.csv").await;
    assert!(csv.headers["content-type"].to_str().unwrap().starts_with("text/csv"));
    assert!(csv.text().contains("\"'=HYPERLINK(\"\"free nitro\"\")\""), "{}", csv.text());
    let _ = warned;
}

#[tokio::test]
async fn nobody_manages_a_server_they_dont_run() {
    let (w, stubs) = world().await;
    let s = &w.state;
    w.install("g1", "owner").await;
    let bob = Browser::new(s);
    discord_user(&stubs, "222", "bob@example.com", json!([{ "id": "g1", "name": "Builders", "icon": null, "owner": false, "permissions": "2048" }]));
    bob.discord("sign_in", "code-b").await;
    assert_eq!(bob.get("/api/servers/g1").await.status, StatusCode::FORBIDDEN);
    assert_eq!(bob.patch("/api/servers/g1", json!({ "mode": "paused" })).await.status, StatusCode::FORBIDDEN);
    // Nor can he install the bot there.
    let refused = bob.discord("install", "install-2").await;
    assert_eq!(refused.location(), "/servers?error=not_manager");

    // Writes need the CSRF token.
    discord_user(&stubs, "111", "owner@example.com", json!([{ "id": "g1", "name": "Builders", "icon": null, "owner": true, "permissions": "0" }]));
    let owner = Browser::new(s);
    owner.discord("sign_in", "code-o").await;
    let no_csrf = Browser { app: owner.app.clone(), cookies: std::sync::Arc::new(std::sync::Mutex::new(owner.cookies.lock().unwrap().clone())) };
    no_csrf.cookies.lock().unwrap().remove("dg_csrf");
    assert_eq!(no_csrf.patch("/api/servers/g1", json!({ "mode": "enforce" })).await.status, StatusCode::FORBIDDEN);
    let saved = owner.patch("/api/servers/g1", json!({ "mode": "enforce" })).await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text());

    // The operator sees every server and raises an allowance; nobody else can.
    assert_eq!(owner.get("/api/operator/servers").await.status, StatusCode::FORBIDDEN);
    discord_user(&stubs, "333", "op@example.com", json!([]));
    let op = Browser::new(s);
    op.discord("sign_in", "code-op").await;
    let servers = op.get("/api/operator/servers").await.json();
    assert_eq!(servers["servers"][0]["allowance"], 10_000);
    assert_eq!(op.patch("/api/operator/servers/g1", json!({ "monthly_allowance": 250000 })).await.status, StatusCode::NO_CONTENT);
    assert_eq!(op.get("/api/servers/g1").await.json()["month"]["allowance"], 250000);
}
