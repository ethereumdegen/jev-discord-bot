//! Accounts, sessions and CSRF (the Degen Builders pattern), and which
//! servers an account may manage.

use axum::http::{HeaderMap, HeaderValue, header::COOKIE};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    state::AppState,
};

pub const CSRF_COOKIE: &str = "dg_csrf";
pub const OAUTH_COOKIE: &str = "dg_oauth";
const SESSION_DAYS: i32 = 30;

#[derive(Debug, Clone, FromRow)]
pub struct Session {
    pub session_id: Uuid,
    pub account_id: Uuid,
    pub name: String,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub discord_user_id: Option<String>,
    pub discord_username: Option<String>,
    pub is_operator: bool,
    pub csrf_hash: Vec<u8>,
}

pub fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}

pub fn hash(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get_all(COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name && !value.is_empty()).then(|| value.to_owned())
    })
}

pub fn safe_return(path: &str) -> String {
    if path.starts_with('/') && !path.starts_with("//") && !path.contains('\\') { path.to_owned() } else { "/".into() }
}

fn session_cookie(state: &AppState) -> &'static str {
    if state.config.production { "__Host-dg_session" } else { "dg_session" }
}

fn secure(state: &AppState) -> &'static str {
    if state.config.production { "; Secure" } else { "" }
}

pub async fn current(state: &AppState, headers: &HeaderMap) -> ApiResult<Option<Session>> {
    let Some(token) = cookie(headers, session_cookie(state)) else { return Ok(None) };
    Ok(sqlx::query_as::<_, Session>(
        "SELECT s.id AS session_id,a.id AS account_id,a.name,a.email,a.avatar_url,a.discord_user_id,a.discord_username,a.is_operator,s.csrf_hash
         FROM sessions s JOIN accounts a ON a.id=s.account_id WHERE s.token_hash=$1 AND s.revoked_at IS NULL AND s.expires_at>now()",
    )
    .bind(hash(&token))
    .fetch_optional(&state.pool)
    .await?)
}

pub async fn require(state: &AppState, headers: &HeaderMap) -> ApiResult<Session> {
    current(state, headers).await?.ok_or(ApiError::Unauthorized)
}

/// A signed-in account that sent the CSRF token: what every write needs.
pub async fn require_writer(state: &AppState, headers: &HeaderMap) -> ApiResult<Session> {
    let session = require(state, headers).await?;
    let header = headers.get("x-csrf-token").and_then(|v| v.to_str().ok()).ok_or(ApiError::Forbidden)?;
    let cookie_value = cookie(headers, CSRF_COOKIE).ok_or(ApiError::Forbidden)?;
    if header != cookie_value || hash(header) != session.csrf_hash {
        return Err(ApiError::Forbidden);
    }
    Ok(session)
}

/// This account may manage this server: Discord said so when they last
/// connected, or they're an operator.
pub async fn require_manager(state: &AppState, session: &Session, guild_id: &str) -> ApiResult<()> {
    if session.is_operator {
        return Ok(());
    }
    let allowed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM account_guilds WHERE account_id=$1 AND guild_id=$2)")
        .bind(session.account_id)
        .bind(guild_id)
        .fetch_one(&state.pool)
        .await?;
    if allowed { Ok(()) } else { Err(ApiError::Forbidden) }
}

/// Who someone is, as a provider reports it.
pub struct Identity {
    pub provider: &'static str,
    pub subject: String,
    pub email: Option<String>,
    pub name: String,
    pub avatar_url: Option<String>,
}

/// Find the account for this identity (or one with the same verified email),
/// or make one. Operators are named by OPERATOR_EMAILS.
pub async fn account_for(state: &AppState, identity: &Identity) -> ApiResult<Uuid> {
    let mut tx = state.pool.begin().await?;
    let existing = sqlx::query_scalar::<_, Uuid>("SELECT account_id FROM account_identities WHERE provider=$1 AND subject=$2")
        .bind(identity.provider)
        .bind(&identity.subject)
        .fetch_optional(&mut *tx)
        .await?;
    let id = match existing {
        Some(id) => id,
        None => {
            let by_email = match &identity.email {
                Some(email) => sqlx::query_scalar::<_, Uuid>("SELECT id FROM accounts WHERE lower(email)=lower($1)").bind(email).fetch_optional(&mut *tx).await?,
                None => None,
            };
            let id = match by_email {
                Some(id) => id,
                None => {
                    let id = Uuid::new_v4();
                    sqlx::query("INSERT INTO accounts(id,email,name,avatar_url) VALUES($1,$2,$3,$4)")
                        .bind(id)
                        .bind(&identity.email)
                        .bind(&identity.name)
                        .bind(&identity.avatar_url)
                        .execute(&mut *tx)
                        .await?;
                    id
                }
            };
            sqlx::query("INSERT INTO account_identities(provider,subject,account_id) VALUES($1,$2,$3)")
                .bind(identity.provider)
                .bind(&identity.subject)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            id
        }
    };
    if let Some(email) = &identity.email
        && state.config.operator_emails.contains(&email.to_lowercase())
    {
        sqlx::query("UPDATE accounts SET is_operator=true WHERE id=$1").bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(id)
}

pub struct NewSession {
    pub token: String,
    pub csrf: String,
}

pub async fn open_session(state: &AppState, account_id: Uuid) -> ApiResult<NewSession> {
    let token = random_token();
    let csrf = random_token();
    sqlx::query("INSERT INTO sessions(id,account_id,token_hash,csrf_hash,expires_at) VALUES($1,$2,$3,$4,now()+make_interval(days=>$5))")
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(hash(&token))
        .bind(hash(&csrf))
        .bind(SESSION_DAYS)
        .execute(&state.pool)
        .await?;
    Ok(NewSession { token, csrf })
}

pub fn session_cookies(state: &AppState, session: &NewSession) -> Vec<HeaderValue> {
    let (secure, max_age) = (secure(state), SESSION_DAYS * 86_400);
    [
        format!("{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}", session_cookie(state), session.token),
        format!("{CSRF_COOKIE}={}; Path=/; SameSite=Lax; Max-Age={max_age}{secure}", session.csrf),
        format!("{OAUTH_COOKIE}=; Path=/api/auth; HttpOnly; SameSite=Lax; Max-Age=0{secure}"),
    ]
    .into_iter()
    .filter_map(|v| HeaderValue::from_str(&v).ok())
    .collect()
}

pub fn clear_cookies(state: &AppState) -> Vec<HeaderValue> {
    let secure = secure(state);
    [format!("{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}", session_cookie(state)), format!("{CSRF_COOKIE}=; Path=/; SameSite=Lax; Max-Age=0{secure}")]
        .into_iter()
        .filter_map(|v| HeaderValue::from_str(&v).ok())
        .collect()
}

pub fn oauth_cookie(state: &AppState, browser: &str) -> HeaderValue {
    HeaderValue::from_str(&format!("{OAUTH_COOKIE}={browser}; Path=/api/auth; HttpOnly; SameSite=Lax; Max-Age=600{}", secure(state))).expect("header-safe")
}
