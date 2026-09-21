//! A throwaway Postgres (pgtemp) and Redis (a `redis-server` on a free port),
//! stand-ins for Jev and Discord that record every call, and a browser with
//! a cookie jar.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    http::{Method, Request, StatusCode, Uri, header},
    response::IntoResponse,
    routing::post,
};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use jev_discord_bot::{
    config::{Config, DiscordConfig, SsoConfig},
    guilds,
    hot::Hot,
    http,
    rules::Incoming,
    state::{AppState, MIGRATOR},
};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

pub struct World {
    pub state: AppState,
    pub discord: Calls,
    pub builders: Calls,
    _pg: pgtemp::PgTempDB,
    _redis: RedisServer,
}

pub struct RedisServer(tokio::process::Child, PathBuf);

impl Drop for RedisServer {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

async fn redis_server() -> (String, RedisServer) {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let dir = std::env::temp_dir().join(format!("jevmod-redis-{port}"));
    std::fs::create_dir_all(&dir).unwrap();
    let child = tokio::process::Command::new("redis-server")
        .args(["--port", &port.to_string(), "--bind", "127.0.0.1", "--save", "", "--appendonly", "no", "--dir"])
        .arg(&dir)
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("redis-server on PATH");
    let url = format!("redis://127.0.0.1:{port}");
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    (url, RedisServer(child, dir))
}

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

/// Degen Builders as the identity provider: `/api/v1/sso/authorize` hands a
/// code straight back (or `login_required` when `data["signed_out"]` is set),
/// `/api/v1/sso/token` swaps it for the identity in `data["builders_user"]`.
async fn builders(data: Arc<Mutex<HashMap<String, Value>>>) -> (String, Calls) {
    let calls: Calls = Arc::default();
    let (recorded, authorize_data, token_data) = (calls.clone(), data.clone(), data);
    let router = Router::new()
        .route(
            "/api/v1/sso/authorize",
            axum::routing::get(move |uri: Uri| {
                let data = authorize_data.clone();
                async move {
                    let query: HashMap<String, String> = url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()).into_owned().collect();
                    let (redirect_uri, state) = (query["redirect_uri"].clone(), query.get("state").cloned().unwrap_or_default());
                    let signed_out = data.lock().unwrap().get("signed_out").is_some();
                    let back = if signed_out {
                        format!("{redirect_uri}?error=login_required&state={state}")
                    } else {
                        format!("{redirect_uri}?code=sso-code-1&state={state}")
                    };
                    (StatusCode::FOUND, [(header::LOCATION, back)])
                }
            }),
        )
        .route(
            "/api/v1/sso/token",
            post(move |Json(body): Json<Value>| {
                let (data, recorded) = (token_data.clone(), recorded.clone());
                async move {
                    recorded.lock().unwrap().push(("POST".into(), "/api/v1/sso/token".into(), body.to_string()));
                    if body["client_secret"] != json!("builders-secret-for-tests") || body["code"] != json!("sso-code-1") {
                        return StatusCode::FORBIDDEN.into_response();
                    }
                    let identity = data.lock().unwrap().get("builders_user").cloned().unwrap_or_else(
                        || json!({ "subject": "11111111-1111-4111-8111-111111111111", "email": "builder@example.com", "handle": "builder", "avatar_url": null }),
                    );
                    Json(json!({ "identity": identity })).into_response()
                }
            }),
        );
    (serve(router).await, calls)
}

pub type Calls = Arc<Mutex<Vec<(String, String, String)>>>;

pub fn count(calls: &Calls, method: &str, path: &str) -> usize {
    calls.lock().unwrap().iter().filter(|(m, p, _)| m == method && p.contains(path)).count()
}

pub fn bodies(calls: &Calls, method: &str, path: &str) -> Vec<String> {
    calls.lock().unwrap().iter().filter(|(m, p, _)| m == method && p.contains(path)).map(|(_, _, b)| b.clone()).collect()
}

/// Discord: OAuth and the bot's REST calls. User 666 outranks the bot (403 on kick/ban).
/// The OAuth user is `data["me"]`, their servers `data["guilds"]`.
async fn discord(data: Arc<Mutex<HashMap<String, Value>>>) -> (String, Calls) {
    let calls: Calls = Arc::default();
    let recorded = calls.clone();
    let router = Router::new().fallback(move |method: Method, uri: Uri, body: Bytes| {
        let recorded = recorded.clone();
        let data = data.clone();
        async move {
            let path = uri.path().to_owned();
            recorded.lock().unwrap().push((method.to_string(), path.clone(), String::from_utf8_lossy(&body).into_owned()));
            let get = |k: &str| data.lock().unwrap().get(k).cloned().unwrap_or(Value::Null);
            match (method, path.as_str()) {
                (Method::POST, "/oauth2/token") => {
                    let form: HashMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
                    let guild = if form.get("code").is_some_and(|c| c.starts_with("install")) { json!({ "id": "g1", "name": "Builders" }) } else { Value::Null };
                    Json(json!({ "access_token": "user-token", "guild": guild })).into_response()
                }
                (Method::GET, "/users/@me") => Json(get("me")).into_response(),
                (Method::GET, "/users/@me/guilds") => Json(get("guilds")).into_response(),
                (Method::GET, "/guilds/g1/channels") => Json(json!([{ "id": "c2", "name": "mod-log", "type": 0, "position": 2 }, { "id": "c1", "name": "general", "type": 0, "position": 1 }, { "id": "v", "name": "voice", "type": 2, "position": 0 }])).into_response(),
                (Method::GET, "/guilds/g1/roles") => Json(json!([{ "id": "g1", "name": "@everyone", "position": 0 }, { "id": "r-mods", "name": "Mods", "position": 5 }, { "id": "r-bot", "name": "Guard", "position": 9, "managed": true }])).into_response(),
                (Method::GET, p) if p.starts_with("/channels/") => Json(json!({ "name": "general" })).into_response(),
                (Method::POST, p) if p.ends_with("/messages") => Json(json!({ "id": "posted-1" })).into_response(),
                (Method::PUT | Method::DELETE, p) if p.contains("/666") => (StatusCode::FORBIDDEN, Json(json!({ "message": "Missing Permissions", "code": 50013 }))).into_response(),
                _ => StatusCode::NO_CONTENT.into_response(),
            }
        }
    });
    (serve(router).await, calls)
}

pub struct Stubs {
    pub data: Arc<Mutex<HashMap<String, Value>>>,
}

impl Stubs {
    pub fn set(&self, key: &str, value: Value) {
        self.data.lock().unwrap().insert(key.to_owned(), value);
    }
}

pub async fn world() -> (World, Stubs) {
    unsafe { std::env::set_var("LC_ALL", "C") };
    let _ = tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_test_writer().try_init();
    let pg =pgtemp::PgTempDB::builder().with_config_param("listen_addresses", "127.0.0.1").start_async().await;
    let pool = PgPoolOptions::new().max_connections(10).connect(&pg.connection_uri().replace("localhost", "127.0.0.1")).await.unwrap();
    MIGRATOR.run(&pool).await.unwrap();
    let (redis_url, redis) = redis_server().await;
    let data: Arc<Mutex<HashMap<String, Value>>> = Arc::default();
    let (discord_base, calls) = discord(data.clone()).await;
    let (builders_base, builders_calls) = builders(data.clone()).await;
    let config = Config {
        brand: "Degen Guard".into(),
        base_url: "http://localhost:3120".into(),
        bind: "127.0.0.1:0".parse().unwrap(),
        production: false,
        database_url: String::new(),
        redis_url: redis_url.clone(),
        static_dir: PathBuf::from("frontend/dist"),
        discord: DiscordConfig {
            client_id: "app1".into(),
            client_secret: "secret".into(),
            bot_token: "bot".into(),
            api_base: discord_base,
            authorize_base: "https://discord.test".into(),
        },
        sso: Some(SsoConfig {
            base_url: builders_base,
            client_id: "degen-guard".into(),
            client_secret: "builders-secret-for-tests".into(),
            label: "Degen Builders".into(),
        }),
        typesafe_api_key: "ts_test".into(),
        typesafe_endpoint: Some(jev().await),
        operator_emails: vec!["op@example.com".into()],
        default_allowance: 10_000,
    };
    let hot = Hot::connect(&redis_url).await.unwrap();
    let state = AppState::new(config, pool, hot).unwrap();
    (World { state, discord: calls, builders: builders_calls, _pg: pg, _redis: redis }, Stubs { data })
}

impl World {
    /// Install the bot in a server (as the gateway does when it joins).
    pub async fn install(&self, guild_id: &str, owner: &str) {
        guilds::installed(&self.state.pool, &self.state.hot, guild_id, "Builders", None, Some(owner), self.state.config.default_allowance).await.unwrap();
    }

    pub async fn set(&self, guild_id: &str, sql_set: &str) {
        sqlx::query(&format!("UPDATE guilds SET {sql_set} WHERE id=$1")).bind(guild_id).execute(&self.state.pool).await.unwrap();
        guilds::forget(&self.state.hot, guild_id).await.unwrap();
    }

    pub async fn outcome(&self, id: uuid::Uuid) -> (String, Option<i32>, bool, Option<String>) {
        sqlx::query_as("SELECT outcome,strike_number,enforced,error FROM actions WHERE id=$1").bind(id).fetch_one(&self.state.pool).await.unwrap()
    }

}

pub fn message(guild: &str, id: &str, author: &str, text: &str, roles: &[&str], joined_days: i64) -> Incoming {
    Incoming {
        guild_id: guild.into(),
        message_id: id.into(),
        channel_id: "c1".into(),
        author_id: author.into(),
        username: format!("u{author}"),
        content: text.into(),
        author_is_bot: false,
        joined_at: Some(Utc::now() - Duration::days(joined_days)),
        role_ids: roles.iter().map(|r| (*r).to_owned()).collect(),
        mentions_everyone: false,
    }
}

/// A browser: keeps cookies, sends the CSRF header on writes.
#[derive(Clone)]
pub struct Browser {
    pub app: Router,
    pub cookies: Arc<Mutex<HashMap<String, String>>>,
    /// Nobody is signed in at degenbuilders.com in this browser.
    pub signed_out_there: Arc<std::sync::atomic::AtomicBool>,
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Bytes,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    pub fn location(&self) -> String {
        self.headers.get(header::LOCATION).map(|v| v.to_str().unwrap().to_owned()).unwrap_or_default()
    }
}

impl Browser {
    pub fn new(state: &AppState) -> Self {
        Self { app: http::app(state.clone()), cookies: Arc::default(), signed_out_there: Arc::default() }
    }

    pub async fn send(&self, method: Method, path: &str, body: Option<Value>) -> Reply {
        let mut request = Request::builder().method(method.clone()).uri(path);
        let jar = self.cookies.lock().unwrap().clone();
        if !jar.is_empty() {
            request = request.header(header::COOKIE, jar.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; "));
        }
        if let (true, Some(csrf)) = (method != Method::GET, jar.get("dg_csrf")) {
            request = request.header("x-csrf-token", csrf);
        }
        let request = match body {
            Some(body) => request.header(header::CONTENT_TYPE, "application/json").body(Body::from(body.to_string())).unwrap(),
            None => request.body(Body::empty()).unwrap(),
        };
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        for value in headers.get_all(header::SET_COOKIE) {
            let raw = value.to_str().unwrap();
            let (pair, _) = raw.split_once(';').unwrap_or((raw, ""));
            let (name, value) = pair.split_once('=').unwrap();
            if value.is_empty() || raw.contains("Max-Age=0") {
                self.cookies.lock().unwrap().remove(name);
            } else {
                self.cookies.lock().unwrap().insert(name.to_owned(), value.to_owned());
            }
        }
        let body = response.into_body().collect().await.unwrap().to_bytes();
        Reply { status, headers, body }
    }

    pub async fn get(&self, path: &str) -> Reply {
        self.send(Method::GET, path, None).await
    }
    pub async fn post(&self, path: &str, body: Value) -> Reply {
        self.send(Method::POST, path, Some(body)).await
    }
    pub async fn patch(&self, path: &str, body: Value) -> Reply {
        self.send(Method::PATCH, path, Some(body)).await
    }

    /// Continue with Degen Builders: start the hand-off, then come back the way
    /// degenbuilders.com would (with a code, or `login_required` when signed out
    /// there and the check was silent).
    pub async fn sso(&self, return_to: &str, silent: bool) -> Reply {
        let query = format!("return_to={return_to}{}", if silent { "&silent=1" } else { "" });
        let started = self.get(&format!("/api/auth/sso/start?{query}")).await;
        assert_eq!(started.status, StatusCode::FOUND, "{}", started.text());
        let location = started.location();
        if location.starts_with('/') {
            return started; // already signed in here: a silent check went straight back
        }
        let url = url::Url::parse(&location).unwrap();
        assert_eq!(url.path(), "/api/v1/sso/authorize");
        let pairs: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs["client_id"], "degen-guard");
        assert_eq!(pairs.get("prompt").map(String::as_str), silent.then_some("none"));
        let state = &pairs["state"];
        let answer = if silent && self.signed_out_there.load(std::sync::atomic::Ordering::Relaxed) {
            format!("error=login_required&state={state}")
        } else {
            format!("code=sso-code-1&state={state}")
        };
        self.get(&format!("/api/auth/sso/callback?{answer}")).await
    }

    /// Continue with Discord (purpose sign_in/connect/install), as the stub user in `stubs`.
    pub async fn discord(&self, purpose: &str, code: &str) -> Reply {
        let started = self.get(&format!("/api/auth/discord/start?purpose={purpose}&return_to=/servers")).await;
        assert_eq!(started.status, StatusCode::FOUND, "{}", started.text());
        let url = url::Url::parse(&started.location()).unwrap();
        let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.into_owned();
        self.get(&format!("/api/auth/discord/callback?code={code}&state={state}")).await
    }
}

pub fn discord_user(stubs: &Stubs, id: &str, email: &str, guilds: Value) {
    stubs.set("me", json!({ "id": id, "username": format!("user{id}"), "global_name": null, "email": email, "verified": true, "avatar": null }));
    stubs.set("guilds", guilds);
}

/// Who degenbuilders.com says is signing in: the only way into this site.
pub fn builders_user(stubs: &Stubs, subject: &str, email: &str, handle: &str) {
    stubs.set("builders_user", json!({ "subject": subject, "email": email, "handle": handle, "avatar_url": null }));
}
