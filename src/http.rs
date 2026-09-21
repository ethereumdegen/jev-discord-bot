//! The website's API (and the built React app for everything else).

use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::CompressionLayer,
    services::{ServeDir, ServeFile},
    set_header::SetResponseHeaderLayer,
    trace::TraceLayer,
};
use uuid::Uuid;

use crate::{
    auth,
    engine,
    error::{ApiError, ApiResult},
    guilds::{self, GUILD_COLUMNS, Guild},
    hot::{month, usage_key},
    oauth,
    rules::parse_ladder,
    state::AppState,
};

const CSP: &str = "default-src 'self'; img-src 'self' data: https://cdn.discordapp.com https://*.googleusercontent.com; connect-src 'self'; \
    style-src 'self'; script-src 'self'; font-src 'self'; frame-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'";

pub fn app(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        .route("/auth/sso/start", get(sso_start))
        .route("/auth/sso/callback", get(sso_callback))
        .route("/auth/discord/start", get(discord_start))
        .route("/auth/discord/callback", get(discord_callback))
        .route("/auth/logout", post(logout))
        .route("/me", get(me))
        .route("/servers", get(servers))
        .route("/servers/{id}", get(server).patch(update_server))
        .route("/servers/{id}/discord", get(server_discord))
        .route("/servers/{id}/rules/{rule}", patch(update_rule))
        .route("/servers/{id}/actions", get(actions))
        .route("/servers/{id}/actions.csv", get(actions_csv))
        .route("/servers/{id}/actions/{action}/undo", post(undo))
        .route("/servers/{id}/actions/{action}/confirm", post(confirm))
        .route("/servers/{id}/users/{user}", get(user_history))
        .route("/operator/servers", get(operator_servers))
        .route("/operator/servers/{id}", patch(operator_update))
        .fallback(|| async { ApiError::NotFound })
        .with_state(state.clone());
    let spa = ServeDir::new(&state.config.static_dir).fallback(ServeFile::new(state.config.static_dir.join("index.html")));
    Router::new()
        .nest("/api", api)
        .fallback_service(spa)
        .layer(SetResponseHeaderLayer::if_not_present(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP)))
        .layer(SetResponseHeaderLayer::if_not_present(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::if_not_present(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")))
        .layer(SetResponseHeaderLayer::if_not_present(header::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin")))
        .layer(CompressionLayer::new())
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http())
}

fn redirect(location: &str, cookies: Vec<HeaderValue>) -> Response {
    let mut response = StatusCode::FOUND.into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(header::LOCATION, value);
    }
    for cookie in cookies {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    response
}

async fn health(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    sqlx::query("SELECT 1").execute(&state.pool).await?;
    if !state.hot.ping().await {
        return Err(ApiError::Unavailable("Redis"));
    }
    Ok(Json(json!({ "ok": true })))
}

// Sign-in ----------------------------------------------------------------------------------

#[derive(Deserialize)]
struct StartQuery {
    return_to: Option<String>,
    /// For SSO: `1` never shows a sign-in screen, it only asks.
    silent: Option<String>,
    /// For Discord: sign_in (default), connect or install.
    purpose: Option<String>,
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    guild_id: Option<String>,
}

/// Off to degenbuilders.com to be signed in. `silent=1` asks whether there's
/// already a session there and comes straight back either way, so landing here
/// signed in over there signs you in here too.
async fn sso_start(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<StartQuery>) -> ApiResult<Response> {
    let return_to = q.return_to.as_deref().unwrap_or("/servers");
    let silent = q.silent.as_deref() == Some("1");
    // Already signed in: a silent check has nothing to do.
    if silent && auth::current(&state, &headers).await?.is_some() {
        return Ok(redirect(&auth::safe_return(return_to), vec![]));
    }
    let started = oauth::begin_sso(&state, return_to, silent).await?;
    Ok(redirect(&started.url, vec![auth::oauth_cookie(&state, &started.browser)]))
}

async fn sso_callback(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<CallbackQuery>) -> Response {
    // `login_required` is the silent check's answer: nobody is signed in over
    // there. Land back where we started, marked so the page doesn't try again.
    if let (Some(error), Some(oauth_state)) = (q.error.as_deref(), q.state.as_deref()) {
        let back = oauth::abandon_sso(&state, &headers, oauth_state).await.unwrap_or_else(|_| "/".into());
        let mark = if error == "login_required" { "sso=none" } else { "error=sso" };
        return redirect(&with_query(&back, mark), vec![]);
    }
    let (Some(code), Some(oauth_state)) = (q.code.as_deref(), q.state.as_deref()) else { return redirect("/?error=sso", vec![]) };
    match oauth::finish_sso(&state, &headers, code, oauth_state).await {
        Ok((path, session)) => redirect(&path, auth::session_cookies(&state, &session)),
        Err(error) => {
            tracing::warn!(?error, "Degen Builders sign-in failed");
            redirect("/?error=sso", vec![])
        }
    }
}

fn with_query(path: &str, pair: &str) -> String {
    if path.contains('?') { format!("{path}&{pair}") } else { format!("{path}?{pair}") }
}

async fn discord_start(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<StartQuery>) -> ApiResult<Response> {
    let session = auth::current(&state, &headers).await?;
    let purpose = match (q.purpose.as_deref(), &session) {
        (Some("install"), _) => "install",
        (_, Some(_)) => "connect",
        _ => "sign_in",
    };
    let started = oauth::begin_discord(&state, purpose, session.map(|s| s.account_id), q.return_to.as_deref().unwrap_or("/servers")).await?;
    Ok(redirect(&started.url, vec![auth::oauth_cookie(&state, &started.browser)]))
}

async fn discord_callback(State(state): State<AppState>, headers: HeaderMap, Query(q): Query<CallbackQuery>) -> Response {
    let (Some(code), Some(oauth_state), None) = (q.code.as_deref(), q.state.as_deref(), q.error.as_deref()) else { return redirect("/servers?error=discord", vec![]) };
    match oauth::finish_discord(&state, &headers, code, oauth_state, q.guild_id.as_deref()).await {
        Ok(done) => redirect(&done.return_path, done.session.map(|s| auth::session_cookies(&state, &s)).unwrap_or_default()),
        Err(ApiError::Forbidden) => redirect("/servers?error=not_manager", vec![]),
        Err(error) => {
            tracing::warn!(?error, "Discord sign-in failed");
            redirect("/servers?error=discord", vec![])
        }
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Response> {
    let session = auth::require_writer(&state, &headers).await?;
    sqlx::query("UPDATE sessions SET revoked_at=now() WHERE id=$1").bind(session.session_id).execute(&state.pool).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    for cookie in auth::clear_cookies(&state) {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let sso = state.config.sso.as_ref();
    let site = json!({ "brand": state.config.brand, "sso": sso.is_some(), "sso_label": sso.map(|s| s.label.clone()), "sso_url": sso.map(|s| s.base_url.clone()) });
    let Some(s) = auth::current(&state, &headers).await? else { return Ok(Json(json!({ "authenticated": false, "site": site }))) };
    Ok(Json(json!({
        "authenticated": true,
        "site": site,
        "account": { "id": s.account_id, "name": s.name, "email": s.email, "avatar_url": s.avatar_url,
                     "discord_username": s.discord_username, "discord_connected": s.discord_user_id.is_some(), "is_operator": s.is_operator },
    })))
}

// Servers ----------------------------------------------------------------------------------

/// The servers this account can manage: the ones with the bot, and the ones it could add it to.
async fn servers(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let s = auth::require(&state, &headers).await?;
    let rows = sqlx::query_scalar::<_, Value>(
        "SELECT COALESCE(json_agg(t ORDER BY t.installed DESC, t.name),'[]'::json) FROM (
           SELECT ag.guild_id AS id,ag.name,ag.icon,ag.owner,(g.id IS NOT NULL AND g.removed_at IS NULL) AS installed,g.mode
           FROM account_guilds ag LEFT JOIN guilds g ON g.id=ag.guild_id WHERE ag.account_id=$1) t",
    )
    .bind(s.account_id)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(json!({ "servers": rows, "discord_connected": s.discord_user_id.is_some() })))
}

async fn load_guild(state: &AppState, headers: &HeaderMap, id: &str, write: bool) -> ApiResult<(auth::Session, Guild)> {
    let session = if write { auth::require_writer(state, headers).await? } else { auth::require(state, headers).await? };
    auth::require_manager(state, &session, id).await?;
    let guild = sqlx::query_as::<_, Guild>(&format!("SELECT {GUILD_COLUMNS} FROM guilds WHERE id=$1")).bind(id).fetch_optional(&state.pool).await?.ok_or(ApiError::NotFound)?;
    Ok((session, guild))
}

async fn server(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let (_, guild) = load_guild(&state, &headers, &id, false).await?;
    let rules = sqlx::query_scalar::<_, Value>("SELECT COALESCE(json_agg(r ORDER BY r.created_at),'[]'::json) FROM (SELECT id,kind,name,enabled,ladder,created_at FROM rules WHERE guild_id=$1) r")
        .bind(&id)
        .fetch_one(&state.pool)
        .await?;
    let stats = sqlx::query_scalar::<_, Value>(
        "SELECT json_build_object(
           'review', count(*) FILTER (WHERE outcome='review' AND reversed_at IS NULL),
           'strikes', count(*) FILTER (WHERE outcome<>'review'),
           'warns', count(*) FILTER (WHERE outcome='warn' AND enforced),
           'kicks', count(*) FILTER (WHERE outcome='kick' AND enforced),
           'bans', count(*) FILTER (WHERE outcome='ban' AND enforced),
           'undone', count(*) FILTER (WHERE reversed_at IS NOT NULL),
           'failed', count(*) FILTER (WHERE error IS NOT NULL))
         FROM actions WHERE guild_id=$1 AND created_at>=date_trunc('month',now())",
    )
    .bind(&id)
    .fetch_one(&state.pool)
    .await?;
    let used = state.hot.counter(&usage_key(&id, &month(Utc::now()))).await?;
    Ok(Json(json!({ "server": guild, "rules": rules, "month": { "judged": used, "allowance": guild.monthly_allowance, "actions": stats } })))
}

/// Channels and roles, for the settings pickers.
async fn server_discord(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    load_guild(&state, &headers, &id, false).await?;
    let channels = state.discord.text_channels(&id).await.map_err(|_| ApiError::Unavailable("Discord"))?;
    let roles = state.discord.roles(&id).await.map_err(|_| ApiError::Unavailable("Discord"))?;
    let pairs = |list: Vec<(String, String)>| list.into_iter().map(|(id, name)| json!({ "id": id, "name": name })).collect::<Vec<_>>();
    Ok(Json(json!({ "channels": pairs(channels), "roles": pairs(roles) })))
}

#[derive(Deserialize)]
struct ServerInput {
    mode: Option<String>,
    log_channel_id: Option<Option<String>>,
    exempt_role_ids: Option<Vec<String>>,
    exempt_channel_ids: Option<Vec<String>>,
    confident_percent: Option<i32>,
    flag_percent: Option<i32>,
    strike_days: Option<i32>,
    timeout_minutes: Option<i32>,
    community: Option<String>,
}

async fn update_server(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(input): Json<ServerInput>) -> ApiResult<Json<Value>> {
    let (_, guild) = load_guild(&state, &headers, &id, true).await?;
    if input.mode.as_deref().is_some_and(|m| !matches!(m, "watch" | "enforce" | "paused")) {
        return Err(ApiError::Validation("Mode is watch, enforce or paused.".into()));
    }
    let confident = input.confident_percent.unwrap_or(guild.confident_percent);
    let flag = input.flag_percent.unwrap_or(guild.flag_percent);
    if !(1..=100).contains(&confident) || !(1..=100).contains(&flag) || flag > confident {
        return Err(ApiError::Validation("Percentages are 1 to 100, and the review line can't be above the action line.".into()));
    }
    if input.strike_days.is_some_and(|d| !(1..=3650).contains(&d)) || input.timeout_minutes.is_some_and(|m| !(1..=40320).contains(&m)) {
        return Err(ApiError::Validation("Strikes last 1 to 3650 days; a time-out is 1 minute to 28 days.".into()));
    }
    if input.community.as_ref().is_some_and(|c| c.len() > 1000) {
        return Err(ApiError::Validation("Keep the description under 1000 characters.".into()));
    }
    let log_channel = match input.log_channel_id {
        Some(value) => value.filter(|v| !v.is_empty()),
        None => guild.log_channel_id.clone(),
    };
    sqlx::query(
        "UPDATE guilds SET mode=COALESCE($2,mode),log_channel_id=$3,exempt_role_ids=COALESCE($4,exempt_role_ids),exempt_channel_ids=COALESCE($5,exempt_channel_ids),
           confident_percent=$6,flag_percent=$7,strike_days=COALESCE($8,strike_days),timeout_minutes=COALESCE($9,timeout_minutes),community=COALESCE($10,community),updated_at=now()
         WHERE id=$1",
    )
    .bind(&id)
    .bind(&input.mode)
    .bind(&log_channel)
    .bind(&input.exempt_role_ids)
    .bind(&input.exempt_channel_ids)
    .bind(confident)
    .bind(flag)
    .bind(input.strike_days)
    .bind(input.timeout_minutes)
    .bind(input.community.as_deref().map(str::trim))
    .execute(&state.pool)
    .await?;
    guilds::forget(&state.hot, &id).await?;
    server(State(state), headers, Path(id)).await
}

#[derive(Deserialize)]
struct RuleInput {
    enabled: Option<bool>,
    ladder: Option<Vec<String>>,
}

async fn update_rule(State(state): State<AppState>, headers: HeaderMap, Path((id, rule)): Path<(String, Uuid)>, Json(input): Json<RuleInput>) -> ApiResult<StatusCode> {
    load_guild(&state, &headers, &id, true).await?;
    if let Some(ladder) = &input.ladder
        && parse_ladder(ladder).is_none()
    {
        return Err(ApiError::Validation("A ladder is 1 to 10 steps, each none, warn, timeout, kick or ban.".into()));
    }
    let updated = sqlx::query("UPDATE rules SET enabled=COALESCE($3,enabled),ladder=COALESCE($4,ladder) WHERE id=$2 AND guild_id=$1")
        .bind(&id)
        .bind(rule)
        .bind(input.enabled)
        .bind(&input.ladder)
        .execute(&state.pool)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    guilds::forget(&state.hot, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// The audit log ----------------------------------------------------------------------------

#[derive(Deserialize)]
struct LogQuery {
    /// review, strikes, warn, timeout, kick, ban, undone, failed
    filter: Option<String>,
    user: Option<String>,
    /// Page by `created_at` of the last row seen.
    before: Option<chrono::DateTime<Utc>>,
    limit: Option<i64>,
}

fn log_where(filter: Option<&str>) -> ApiResult<&'static str> {
    Ok(match filter.unwrap_or("all") {
        "all" => "true",
        "review" => "outcome='review' AND reversed_at IS NULL",
        "strikes" => "outcome<>'review'",
        "warn" => "outcome='warn'",
        "timeout" => "outcome='timeout'",
        "kick" => "outcome='kick'",
        "ban" => "outcome='ban'",
        "undone" => "reversed_at IS NOT NULL",
        "failed" => "error IS NOT NULL",
        _ => return Err(ApiError::Validation("unknown filter".into())),
    })
}

const ACTION_COLUMNS: &str = "id,author_id,username,channel_id,channel_name,message_id,excerpt,verdict,probabilities,confidence,lure,outcome,strike_number,
    enforced,message_deleted,error,created_at,reversed_at,reversed_by,marked_wrong";

async fn actions(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<LogQuery>) -> ApiResult<Json<Value>> {
    load_guild(&state, &headers, &id, false).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let rows = sqlx::query_scalar::<_, Value>(&format!(
        "SELECT COALESCE(json_agg(t ORDER BY t.created_at DESC),'[]'::json) FROM (
           SELECT {ACTION_COLUMNS} FROM actions WHERE guild_id=$1 AND {} AND ($2::text IS NULL OR author_id=$2) AND ($3::timestamptz IS NULL OR created_at<$3)
           ORDER BY created_at DESC LIMIT $4) t",
        log_where(q.filter.as_deref())?
    ))
    .bind(&id)
    .bind(&q.user)
    .bind(q.before)
    .bind(limit)
    .fetch_one(&state.pool)
    .await?;
    Ok(Json(json!({ "actions": rows })))
}

fn csv_field(value: &str) -> String {
    // Quote everything, double quotes inside, and defuse spreadsheet formulas.
    let value = if value.starts_with(['=', '+', '-', '@']) { format!("'{value}") } else { value.to_owned() };
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[derive(sqlx::FromRow)]
struct CsvRow {
    created_at: chrono::DateTime<Utc>,
    author_id: String,
    username: String,
    channel_name: Option<String>,
    excerpt: String,
    verdict: String,
    confidence: f64,
    outcome: String,
    strike_number: Option<i32>,
    enforced: bool,
    error: Option<String>,
    undone: bool,
}

async fn actions_csv(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult<Response> {
    load_guild(&state, &headers, &id, false).await?;
    let rows: Vec<CsvRow> = sqlx::query_as(
        "SELECT created_at,author_id,username,channel_name,excerpt,verdict,confidence,outcome,strike_number,enforced,error,reversed_at IS NOT NULL AS undone
         FROM actions WHERE guild_id=$1 ORDER BY created_at DESC LIMIT 10000",
    )
    .bind(&id)
    .fetch_all(&state.pool)
    .await?;
    let mut out = String::from("when,user_id,username,channel,message,verdict,sure,action,strike,done,error,undone\n");
    for row in rows {
        let fields = [
            row.created_at.to_rfc3339(),
            row.author_id,
            row.username,
            row.channel_name.unwrap_or_default(),
            row.excerpt,
            row.verdict,
            format!("{:.0}%", row.confidence * 100.0),
            row.outcome,
            row.strike_number.map(|s| s.to_string()).unwrap_or_default(),
            row.enforced.to_string(),
            row.error.unwrap_or_default(),
            row.undone.to_string(),
        ];
        out.push_str(&fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    Ok((
        [(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"audit-{id}.csv\""))],
        out,
    )
        .into_response())
}

async fn undo(State(state): State<AppState>, headers: HeaderMap, Path((id, action)): Path<(String, Uuid)>) -> ApiResult<Json<Value>> {
    let (session, _) = load_guild(&state, &headers, &id, true).await?;
    let did = engine::undo(&state, &id, action, &format!("web:{}", session.name)).await?.ok_or(ApiError::Conflict("Already undone.".into()))?;
    Ok(Json(json!({ "did": did })))
}

async fn confirm(State(state): State<AppState>, headers: HeaderMap, Path((id, action)): Path<(String, Uuid)>) -> ApiResult<Json<Value>> {
    load_guild(&state, &headers, &id, true).await?;
    let did = engine::confirm(&state, &id, action).await?.ok_or(ApiError::Conflict("That entry isn't waiting for review.".into()))?;
    Ok(Json(json!({ "did": did })))
}

/// One person's record in this server.
async fn user_history(State(state): State<AppState>, headers: HeaderMap, Path((id, user)): Path<(String, String)>) -> ApiResult<Json<Value>> {
    load_guild(&state, &headers, &id, false).await?;
    let actions = sqlx::query_scalar::<_, Value>(&format!(
        "SELECT COALESCE(json_agg(t ORDER BY t.created_at DESC),'[]'::json) FROM (SELECT {ACTION_COLUMNS} FROM actions WHERE guild_id=$1 AND author_id=$2 ORDER BY created_at DESC LIMIT 200) t"
    ))
    .bind(&id)
    .bind(&user)
    .fetch_one(&state.pool)
    .await?;
    let live: i64 = sqlx::query_scalar("SELECT count(*) FROM strikes WHERE guild_id=$1 AND user_id=$2 AND cleared_at IS NULL AND expires_at>now()")
        .bind(&id)
        .bind(&user)
        .fetch_one(&state.pool)
        .await?;
    let messages = state.hot.counter(&format!("jev:msgs:{id}:{user}")).await?;
    Ok(Json(json!({ "user_id": user, "live_strikes": live, "messages_seen": messages, "actions": actions })))
}

// Operator ---------------------------------------------------------------------------------

async fn operator_servers(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<Json<Value>> {
    let s = auth::require(&state, &headers).await?;
    if !s.is_operator {
        return Err(ApiError::Forbidden);
    }
    let rows: Vec<Guild> = sqlx::query_as(&format!("SELECT {GUILD_COLUMNS} FROM guilds ORDER BY installed_at DESC LIMIT 1000")).fetch_all(&state.pool).await?;
    let this_month = month(Utc::now());
    let mut out = Vec::with_capacity(rows.len());
    for g in rows {
        let used = state.hot.counter(&usage_key(&g.id, &this_month)).await?;
        out.push(json!({ "id": g.id, "name": g.name, "mode": g.mode, "allowance": g.monthly_allowance, "judged": used, "removed_at": g.removed_at }));
    }
    Ok(Json(json!({ "servers": out })))
}

async fn operator_update(State(state): State<AppState>, headers: HeaderMap, Path(id): Path<String>, Json(input): Json<HashMap<String, Value>>) -> ApiResult<StatusCode> {
    let s = auth::require_writer(&state, &headers).await?;
    if !s.is_operator {
        return Err(ApiError::Forbidden);
    }
    let allowance = input.get("monthly_allowance").and_then(Value::as_i64).filter(|a| (0..=10_000_000).contains(a)).ok_or_else(|| ApiError::Validation("monthly_allowance: 0 to 10,000,000".into()))?;
    let updated = sqlx::query("UPDATE guilds SET monthly_allowance=$2,updated_at=now() WHERE id=$1").bind(&id).bind(allowance as i32).execute(&state.pool).await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    guilds::forget(&state.hot, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
