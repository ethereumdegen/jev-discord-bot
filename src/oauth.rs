//! Google sign-in, and three Discord round trips that share one callback:
//! `sign_in` (Continue with Discord), `connect` (a signed-in account links
//! Discord and we learn which servers it manages) and `install` (Add to
//! Discord: the bot joins a server, and the same screen proves who added it).

use axum::http::HeaderMap;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    auth::{self, Identity, NewSession},
    discord::{PartialGuild, User},
    error::{ApiError, ApiResult},
    guilds,
    state::AppState,
};

pub struct Started {
    pub url: String,
    pub browser: String,
}

struct Attempt {
    purpose: String,
    account_id: Option<Uuid>,
    nonce: String,
    pkce_verifier: String,
    return_path: String,
}

async fn begin(state: &AppState, provider: &str, purpose: &str, account_id: Option<Uuid>, return_path: &str) -> ApiResult<(String, String, String, String)> {
    let (oauth_state, browser, nonce, verifier) = (auth::random_token(), auth::random_token(), auth::random_token(), auth::random_token());
    sqlx::query(
        "INSERT INTO oauth_attempts(state_hash,provider,purpose,account_id,browser_hash,nonce,pkce_verifier,return_path,expires_at)
         VALUES($1,$2,$3,$4,$5,$6,$7,$8,now()+interval '10 minutes')",
    )
    .bind(auth::hash(&oauth_state))
    .bind(provider)
    .bind(purpose)
    .bind(account_id)
    .bind(auth::hash(&browser))
    .bind(&nonce)
    .bind(&verifier)
    .bind(auth::safe_return(return_path))
    .execute(&state.pool)
    .await?;
    Ok((oauth_state, browser, nonce, verifier))
}

async fn consume(state: &AppState, headers: &HeaderMap, provider: &str, oauth_state: &str) -> ApiResult<Attempt> {
    let browser = auth::cookie(headers, auth::OAUTH_COOKIE).ok_or(ApiError::Forbidden)?;
    let row: Option<(String, Option<Uuid>, String, String, String)> = sqlx::query_as(
        "UPDATE oauth_attempts SET consumed_at=now() WHERE state_hash=$1 AND browser_hash=$2 AND provider=$3 AND consumed_at IS NULL AND expires_at>now()
         RETURNING purpose,account_id,nonce,pkce_verifier,return_path",
    )
    .bind(auth::hash(oauth_state))
    .bind(auth::hash(&browser))
    .bind(provider)
    .fetch_optional(&state.pool)
    .await?;
    let (purpose, account_id, nonce, pkce_verifier, return_path) = row.ok_or(ApiError::Forbidden)?;
    Ok(Attempt { purpose, account_id, nonce, pkce_verifier, return_path })
}

// Google -------------------------------------------------------------------------------

pub async fn begin_google(state: &AppState, return_path: &str) -> ApiResult<Started> {
    let google = state.config.google.as_ref().ok_or(ApiError::Unavailable("Google sign-in"))?;
    let (oauth_state, browser, nonce, verifier) = begin(state, "google", "sign_in", None, return_path).await?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = url::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").map_err(anyhow::Error::from)?;
    url.query_pairs_mut()
        .append_pair("client_id", &google.client_id)
        .append_pair("redirect_uri", &format!("{}/api/auth/google/callback", state.config.base_url))
        .append_pair("response_type", "code")
        .append_pair("scope", "openid email profile")
        .append_pair("state", &oauth_state)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", "select_account");
    Ok(Started { url: url.into(), browser })
}

#[derive(Deserialize)]
struct GoogleClaims {
    sub: String,
    nonce: String,
    email: Option<String>,
    email_verified: Option<bool>,
    name: Option<String>,
    picture: Option<String>,
}

pub async fn finish_google(state: &AppState, headers: &HeaderMap, code: &str, oauth_state: &str) -> ApiResult<(String, NewSession)> {
    let google = state.config.google.as_ref().ok_or(ApiError::Unavailable("Google sign-in"))?;
    let attempt = consume(state, headers, "google", oauth_state).await?;
    let redirect = format!("{}/api/auth/google/callback", state.config.base_url);
    let response = state
        .http
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", google.client_id.as_str()),
            ("client_secret", google.client_secret.as_str()),
            ("code", code),
            ("code_verifier", attempt.pkce_verifier.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect.as_str()),
        ])
        .send()
        .await
        .map_err(|_| ApiError::Unavailable("Google sign-in"))?;
    if !response.status().is_success() {
        return Err(ApiError::Forbidden);
    }
    let token: serde_json::Value = response.json().await.map_err(|_| ApiError::Forbidden)?;
    let id_token = token["id_token"].as_str().ok_or(ApiError::Forbidden)?;
    let header = decode_header(id_token).map_err(|_| ApiError::Forbidden)?;
    if header.alg != Algorithm::RS256 {
        return Err(ApiError::Forbidden);
    }
    let kid = header.kid.ok_or(ApiError::Forbidden)?;
    let jwks: JwkSet = state
        .http
        .get("https://www.googleapis.com/oauth2/v3/certs")
        .send()
        .await
        .map_err(|_| ApiError::Unavailable("Google sign-in"))?
        .json()
        .await
        .map_err(|_| ApiError::Forbidden)?;
    let key = DecodingKey::from_jwk(jwks.find(&kid).ok_or(ApiError::Forbidden)?).map_err(|_| ApiError::Forbidden)?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[google.client_id.as_str()]);
    validation.set_issuer(&["https://accounts.google.com", "accounts.google.com"]);
    let claims = decode::<GoogleClaims>(id_token, &key, &validation).map_err(|_| ApiError::Forbidden)?.claims;
    if claims.nonce != attempt.nonce || claims.email_verified != Some(true) {
        return Err(ApiError::Forbidden);
    }
    let name = claims.name.clone().or_else(|| claims.email.clone()).unwrap_or_else(|| "Someone".into());
    let identity = Identity { provider: "google", subject: claims.sub, email: claims.email, name, avatar_url: claims.picture };
    let account = auth::account_for(state, &identity).await?;
    Ok((attempt.return_path, auth::open_session(state, account).await?))
}

// Discord ------------------------------------------------------------------------------

fn discord_redirect(state: &AppState) -> String {
    format!("{}/api/auth/discord/callback", state.config.base_url)
}

/// `purpose`: sign_in, connect (needs `account_id`) or install.
pub async fn begin_discord(state: &AppState, purpose: &str, account_id: Option<Uuid>, return_path: &str) -> ApiResult<Started> {
    let (oauth_state, browser, _, _) = begin(state, "discord", purpose, account_id, return_path).await?;
    let url = state.discord.authorize_url(&discord_redirect(state), &oauth_state, purpose == "install");
    Ok(Started { url, browser })
}

pub struct DiscordDone {
    pub return_path: String,
    /// A new session when this was a sign-in.
    pub session: Option<NewSession>,
}

pub async fn finish_discord(state: &AppState, headers: &HeaderMap, code: &str, oauth_state: &str, installed_guild: Option<&str>) -> ApiResult<DiscordDone> {
    let attempt = consume(state, headers, "discord", oauth_state).await?;
    let token = state.discord.exchange_code(code, &discord_redirect(state)).await.map_err(|error| {
        tracing::warn!(?error, "Discord code exchange failed");
        ApiError::Forbidden
    })?;
    let user = state.discord.me(&token.access_token).await.map_err(|_| ApiError::Unavailable("Discord"))?;
    let guilds = state.discord.my_guilds(&token.access_token).await.map_err(|_| ApiError::Unavailable("Discord"))?;
    let (account_id, session) = match attempt.account_id {
        Some(id) => (id, None),
        None => {
            let identity = Identity {
                provider: "discord",
                subject: user.id.clone(),
                email: user.email.clone().filter(|_| user.verified == Some(true)),
                name: user.global_name.clone().unwrap_or_else(|| user.username.clone()),
                avatar_url: user.avatar.as_ref().map(|hash| format!("https://cdn.discordapp.com/avatars/{}/{hash}.png", user.id)),
            };
            let id = auth::account_for(state, &identity).await?;
            (id, Some(auth::open_session(state, id).await?))
        }
    };
    link_discord(state, account_id, &user, &guilds).await?;
    let mut return_path = attempt.return_path;
    if attempt.purpose == "install" {
        let guild_id = token.guild.as_ref().and_then(|g| g["id"].as_str()).map(str::to_owned).or_else(|| installed_guild.map(str::to_owned));
        let Some(guild_id) = guild_id else { return Ok(DiscordDone { return_path: "/servers?install=cancelled".into(), session }) };
        let Some(guild) = guilds.iter().find(|g| g.id == guild_id && g.manageable()) else {
            return Err(ApiError::Forbidden);
        };
        guilds::installed(&state.pool, &state.hot, &guild.id, &guild.name, guild.icon.as_deref(), None, state.config.default_allowance).await?;
        return_path = format!("/servers/{guild_id}?installed=1");
    }
    Ok(DiscordDone { return_path, session })
}

/// Remember this account's Discord identity and the servers it can manage.
async fn link_discord(state: &AppState, account_id: Uuid, user: &User, guilds: &[PartialGuild]) -> ApiResult<()> {
    let mut tx = state.pool.begin().await?;
    // A Discord account belongs to one of our accounts; linking it here moves it.
    sqlx::query("UPDATE accounts SET discord_user_id=NULL WHERE discord_user_id=$1 AND id<>$2").bind(&user.id).bind(account_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE accounts SET discord_user_id=$2,discord_username=$3 WHERE id=$1").bind(account_id).bind(&user.id).bind(&user.username).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO account_identities(provider,subject,account_id) VALUES('discord',$1,$2) ON CONFLICT(provider,subject) DO UPDATE SET account_id=excluded.account_id")
        .bind(&user.id)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM account_guilds WHERE account_id=$1").bind(account_id).execute(&mut *tx).await?;
    for guild in guilds.iter().filter(|g| g.manageable()) {
        sqlx::query("INSERT INTO account_guilds(account_id,guild_id,name,icon,owner) VALUES($1,$2,$3,$4,$5)")
            .bind(account_id)
            .bind(&guild.id)
            .bind(&guild.name)
            .bind(&guild.icon)
            .bind(guild.owner)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
