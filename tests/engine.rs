//! The engine in a real server's shoes: watch vs enforce, the warn → kick →
//! ban ladder, exemptions, the monthly allowance, "couldn't act", strike
//! expiry, undo and confirm, and the queue between the gateway and workers.

mod common;

use common::*;
use jev_discord_bot::{
    engine,
    interactions::{self, InteractionIn, InteractionKind},
    worker,
};
use serde_json::json;

#[tokio::test]
async fn the_ladder_warns_then_kicks_then_bans() {
    let (w, _) = world().await;
    w.install("g1", "owner").await;
    let s = &w.state;

    // New servers watch: a scam is logged as "would warn", nothing is touched.
    let id = engine::handle_message(s, &message("g1", "m1", "500", "FREE NITRO https://nitro.example", &[], 1)).await.unwrap().unwrap();
    assert_eq!(w.outcome(id).await, ("warn".into(), Some(1), false, None));
    assert_eq!(count(&w.discord, "DELETE", "/messages/m1"), 0);
    // Ordinary chat is judged, not logged.
    assert!(engine::handle_message(s, &message("g1", "m2", "501", "what model do you use for evals?", &[], 1)).await.unwrap().is_none());

    w.set("g1", "mode='enforce', log_channel_id='modlog'").await;
    // Strike 1: the message goes, a notice pings them, the log gets a Wrong-call button.
    let a = engine::handle_message(s, &message("g1", "m3", "502", "buy my course, 90% off", &[], 40)).await.unwrap().unwrap();
    assert_eq!(w.outcome(a).await, ("warn".into(), Some(1), true, None));
    assert_eq!(count(&w.discord, "DELETE", "/channels/c1/messages/m3"), 1);
    let notice = bodies(&w.discord, "POST", "/channels/c1/messages");
    assert!(notice.iter().any(|b| b.contains("<@502>") && b.contains("Next time: a kick") && b.contains("\"users\":[\"502\"]")), "{notice:?}");
    let logged = bodies(&w.discord, "POST", "/channels/modlog/messages");
    assert!(logged.last().unwrap().contains("**warn**") && logged.last().unwrap().contains(&format!("g:undo:{a}")), "{logged:?}");

    // Strike 2: kick. Strike 3: ban.
    w.cool_down().await;
    let b = engine::handle_message(s, &message("g1", "m4", "502", "buy my course, last chance", &[], 40)).await.unwrap().unwrap();
    assert_eq!(w.outcome(b).await.0, "kick");
    assert_eq!(count(&w.discord, "DELETE", "/guilds/g1/members/502"), 1);
    w.cool_down().await;
    let c = engine::handle_message(s, &message("g1", "m5", "502", "buy my course!!!", &[], 40)).await.unwrap().unwrap();
    assert_eq!(w.outcome(c).await.0, "ban");
    assert_eq!(count(&w.discord, "PUT", "/guilds/g1/bans/502"), 1);

    // Undo the ban: unbanned, strike cleared, marked wrong; a second undo does nothing.
    assert!(engine::undo(s, "g1", c, "web:mod").await.unwrap().unwrap().contains("unbanned"));
    assert_eq!(count(&w.discord, "DELETE", "/guilds/g1/bans/502"), 1);
    assert!(engine::undo(s, "g1", c, "web:mod").await.unwrap().is_none());
    // So their next offense is strike 3 again, a ban.
    w.cool_down().await;
    let d = engine::handle_message(s, &message("g1", "m6", "502", "buy my course again", &[], 40)).await.unwrap().unwrap();
    assert_eq!(w.outcome(d).await.1, Some(3));

    // Strikes expire.
    sqlx::query("UPDATE strikes SET expires_at=now()-interval '1 day' WHERE user_id='502'").execute(&s.pool).await.unwrap();
    w.cool_down().await;
    let e = engine::handle_message(s, &message("g1", "m7", "502", "buy my course, fresh start", &[], 40)).await.unwrap().unwrap();
    assert_eq!(w.outcome(e).await.1, Some(1));

    // A custom ladder: time out first.
    let rule: uuid::Uuid = sqlx::query_scalar("SELECT id FROM rules WHERE guild_id='g1'").fetch_one(&s.pool).await.unwrap();
    sqlx::query("UPDATE rules SET ladder='{timeout,ban}' WHERE id=$1").bind(rule).execute(&s.pool).await.unwrap();
    jev_discord_bot::guilds::forget(&s.hot, "g1").await.unwrap();
    let f = engine::handle_message(s, &message("g1", "m8", "503", "free nitro https://x.example", &[], 2)).await.unwrap().unwrap();
    assert_eq!(w.outcome(f).await.0, "timeout");
    assert!(bodies(&w.discord, "PATCH", "/guilds/g1/members/503")[0].contains("communication_disabled_until"));
}

#[tokio::test]
async fn skips_exemptions_and_reports_what_it_couldnt_do() {
    let (w, _) = world().await;
    w.install("g1", "owner").await;
    w.set("g1", "mode='enforce', exempt_role_ids='{r-mods}', exempt_channel_ids='{c-promo}'").await;
    let s = &w.state;
    assert!(engine::handle_message(s, &message("g1", "m1", "owner", "free nitro", &[], 900)).await.unwrap().is_none(), "the owner");
    assert!(engine::handle_message(s, &message("g1", "m2", "600", "free nitro", &["r-mods"], 1)).await.unwrap().is_none(), "an exempt role");
    let mut promo = message("g1", "m3", "601", "check out my server https://mine.example", &[], 1);
    promo.channel_id = "c-promo".into();
    assert!(engine::handle_message(s, &promo).await.unwrap().is_none(), "an exempt channel");
    // A regular's plain chat never reaches Jev, but their links do.
    sqlx::query("SELECT 1").execute(&s.pool).await.unwrap();
    for i in 0..25 {
        let _ = engine::handle_message(s, &message("g1", &format!("r{i}"), "602", "hello", &[], 90)).await;
        w.cool_down().await;
    }
    assert!(engine::handle_message(s, &message("g1", "m4", "602", "free nitro lol", &[], 90)).await.unwrap().is_none());
    assert!(engine::handle_message(s, &message("g1", "m5", "602", "free nitro https://x.example", &[], 90)).await.unwrap().is_some());

    // Someone who outranks the bot: logged as couldn't act, no strike.
    sqlx::query("UPDATE rules SET ladder='{kick}'").execute(&s.pool).await.unwrap();
    jev_discord_bot::guilds::forget(&s.hot, "g1").await.unwrap();
    let id = engine::handle_message(s, &message("g1", "m6", "666", "free nitro https://x.example", &[], 1)).await.unwrap().unwrap();
    let (outcome, _, enforced, error) = w.outcome(id).await;
    assert_eq!((outcome.as_str(), enforced), ("kick", false));
    assert!(error.unwrap().contains("couldn't kick"));
    let strikes: i64 = sqlx::query_scalar("SELECT count(*) FROM strikes WHERE user_id='666'").fetch_one(&s.pool).await.unwrap();
    assert_eq!(strikes, 0);

    // Paused: nothing is judged.
    w.set("g1", "mode='paused'").await;
    assert!(engine::handle_message(s, &message("g1", "m7", "603", "free nitro", &[], 1)).await.unwrap().is_none());
}

#[tokio::test]
async fn review_entries_and_the_monthly_allowance() {
    let (w, _) = world().await;
    w.install("g1", "owner").await;
    w.set("g1", "mode='enforce', log_channel_id='modlog', monthly_allowance=3").await;
    let s = &w.state;
    // Borderline: listed for review with Take-action/Dismiss buttons, no strike, nothing deleted.
    let id = engine::handle_message(s, &message("g1", "m1", "700", "check out my server https://mine.example", &[], 2)).await.unwrap().unwrap();
    assert_eq!(w.outcome(id).await, ("review".into(), None, false, None));
    assert!(bodies(&w.discord, "POST", "/channels/modlog/messages")[0].contains(&format!("g:confirm:{id}")));
    // A mod takes action from the review: strike 1, a warning.
    assert!(engine::confirm(s, "g1", id).await.unwrap().unwrap().starts_with("warn"));
    assert_eq!(w.outcome(id).await, ("warn".into(), Some(1), true, None));
    assert!(engine::confirm(s, "g1", id).await.unwrap().is_none(), "only once");

    // Allowance of 3: the 4th judged message isn't judged, and the log is told once.
    for i in 0..3 {
        engine::handle_message(s, &message("g1", &format!("q{i}"), &format!("71{i}"), "hello there", &[], 1)).await.unwrap();
    }
    assert!(engine::handle_message(s, &message("g1", "q9", "719", "free nitro", &[], 1)).await.unwrap().is_none());
    assert!(engine::handle_message(s, &message("g1", "q10", "720", "free nitro", &[], 1)).await.unwrap().is_none());
    let notes = bodies(&w.discord, "POST", "/channels/modlog/messages").into_iter().filter(|b| b.contains("this month's 3 judged messages")).count();
    assert_eq!(notes, 1);
}

#[tokio::test]
async fn the_queue_delivers_each_message_once() {
    let (w, _) = world().await;
    w.install("g1", "owner").await;
    let s = &w.state;
    let payload = serde_json::to_string(&message("g1", "m1", "800", "free nitro https://x.example", &[], 1)).unwrap();
    assert!(s.hot.enqueue("m1", &payload).await.unwrap());
    assert!(!s.hot.enqueue("m1", &payload).await.unwrap(), "a resumed gateway's duplicate");
    let batch = s.hot.next_batch("w1", 10, 100).await.unwrap();
    assert_eq!(batch.len(), 1);
    worker::process(s, &batch[0].0, &batch[0].1).await.unwrap();
    assert!(s.hot.next_batch("w1", 10, 100).await.unwrap().is_empty(), "acknowledged");
    let logged: i64 = sqlx::query_scalar("SELECT count(*) FROM actions").fetch_one(&s.pool).await.unwrap();
    assert_eq!(logged, 1);
}

#[tokio::test]
async fn discord_buttons_and_the_guard_command() {
    let (w, _) = world().await;
    w.install("g1", "owner").await;
    w.set("g1", "mode='enforce'").await;
    let s = &w.state;
    sqlx::query("UPDATE rules SET ladder='{ban}'").execute(&s.pool).await.unwrap();
    jev_discord_bot::guilds::forget(&s.hot, "g1").await.unwrap();
    let id = engine::handle_message(s, &message("g1", "m1", "900", "free nitro https://x.example", &[], 1)).await.unwrap().unwrap();
    let press = |perms: u64| InteractionIn {
        id: "i".into(),
        token: "t".into(),
        guild_id: Some("g1".into()),
        user_id: "mod".into(),
        permissions: perms,
        kind: InteractionKind::Button { custom_id: format!("g:undo:{id}"), message_content: "[report]".into() },
    };
    let nope = interactions::answer(s, &press(0)).await.unwrap();
    assert!(nope["data"]["content"].as_str().unwrap().contains("Only mods"));
    let done = interactions::answer(s, &press(1 << 2)).await.unwrap();
    assert_eq!(done["type"], 7);
    assert!(done["data"]["content"].as_str().unwrap().contains("unbanned"));
    assert_eq!(done["data"]["components"], json!([]));

    let command = |sub: &str, value: Option<&str>, perms: u64| InteractionIn {
        id: "i".into(),
        token: "t".into(),
        guild_id: Some("g1".into()),
        user_id: "boss".into(),
        permissions: perms,
        kind: InteractionKind::Command { subcommand: sub.into(), value: value.map(str::to_owned) },
    };
    assert!(interactions::answer(s, &command("mode", Some("watch"), 0)).await.unwrap()["data"]["content"].as_str().unwrap().contains("Only server managers"));
    assert_eq!(interactions::answer(s, &command("mode", Some("watch"), 1 << 5)).await.unwrap()["data"]["flags"], 64);
    let mode: String = sqlx::query_scalar("SELECT mode FROM guilds WHERE id='g1'").fetch_one(&s.pool).await.unwrap();
    assert_eq!(mode, "watch");
    let status = interactions::answer(s, &command("status", None, 1 << 3)).await.unwrap();
    assert!(status["data"]["content"].as_str().unwrap().contains("**watch**"), "{status}");
}
