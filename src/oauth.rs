//! Sign-in through Degen Builders, and three Discord round trips that share one
//! callback:
//! `sign_in` (Continue with Discord), `connect` (a signed-in account links
//! Discord and we learn which servers it manages) and `install` (Add to
//! Discord: the bot joins a server, and the same screen proves who added it).

use axum::http::HeaderMap;
use serde::Deserialize;
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
    return_path: String,
}

/// Remember a round trip we started, and hand back the `state` the other side
/// echoes and the `browser` value its cookie carries.
async fn begin(state: &AppState, provider: &str, purpose: &str, account_id: Option<Uuid>, return_path: &str) -> ApiResult<(String, String)> {
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
    Ok((oauth_state, browser))
}

async fn consume(state: &AppState, headers: &HeaderMap, provider: &str, oauth_state: &str) -> ApiResult<Attempt> {
    let browser = auth::cookie(headers, auth::OAUTH_COOKIE).ok_or(ApiError::Forbidden)?;
    let row: Option<(String, Option<Uuid>, String)> = sqlx::query_as(
        "UPDATE oauth_attempts SET consumed_at=now() WHERE state_hash=$1 AND browser_hash=$2 AND provider=$3 AND consumed_at IS NULL AND expires_at>now()
         RETURNING purpose,account_id,return_path",
    )
    .bind(auth::hash(oauth_state))
    .bind(auth::hash(&browser))
    .bind(provider)
    .fetch_optional(&state.pool)
    .await?;
    let (purpose, account_id, return_path) = row.ok_or(ApiError::Forbidden)?;
    Ok(Attempt { purpose, account_id, return_path })
}

// Degen Builders -----------------------------------------------------------------------

/// Where a browser goes to be signed in by degenbuilders.com. `silent` asks
/// whether there's already a session there without showing a sign-in screen.
pub async fn begin_sso(state: &AppState, return_path: &str, silent: bool) -> ApiResult<Started> {
    let sso = state.config.sso.as_ref().ok_or(ApiError::Unavailable("Sign-in"))?;
    let (oauth_state, browser) = begin(state, "builders", "sign_in", None, return_path).await?;
    let mut url = url::Url::parse(&format!("{}/api/v1/sso/authorize", sso.base_url)).map_err(anyhow::Error::from)?;
    url.query_pairs_mut()
        .append_pair("client_id", &sso.client_id)
        .append_pair("redirect_uri", &sso_redirect(state))
        .append_pair("state", &oauth_state);
    if silent {
        url.query_pairs_mut().append_pair("prompt", "none");
    }
    Ok(Started { url: url.into(), browser })
}

/// Degen Builders said nobody is signed in there. Burn the attempt and say
/// where the browser was headed, so a silent try lands back where it started.
pub async fn abandon_sso(state: &AppState, headers: &HeaderMap, oauth_state: &str) -> ApiResult<String> {
    Ok(consume(state, headers, "builders", oauth_state).await?.return_path)
}

fn sso_redirect(state: &AppState) -> String {
    format!("{}/api/auth/sso/callback", state.config.base_url)
}

#[derive(Deserialize)]
struct SsoIdentity {
    subject: String,
    email: Option<String>,
    handle: String,
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct SsoToken {
    identity: SsoIdentity,
}

/// Swap the code the browser carried for the identity behind it, over a back
/// channel with our own secret, and open a session here.
pub async fn finish_sso(state: &AppState, headers: &HeaderMap, code: &str, oauth_state: &str) -> ApiResult<(String, NewSession)> {
    let sso = state.config.sso.as_ref().ok_or(ApiError::Unavailable("Sign-in"))?;
    let attempt = consume(state, headers, "builders", oauth_state).await?;
    let response = state
        .http
        .post(format!("{}/api/v1/sso/token", sso.base_url))
        .json(&serde_json::json!({ "client_id": sso.client_id, "client_secret": sso.client_secret, "code": code }))
        .send()
        .await
        .map_err(|_| ApiError::Unavailable("Sign-in"))?;
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "Degen Builders refused the sign-in code");
        return Err(ApiError::Forbidden);
    }
    let token: SsoToken = response.json().await.map_err(|_| ApiError::Forbidden)?;
    let identity = Identity {
        provider: "builders",
        subject: token.identity.subject,
        email: token.identity.email,
        name: token.identity.handle,
        avatar_url: token.identity.avatar_url,
    };
    let account = auth::account_for(state, &identity).await?;
    Ok((attempt.return_path, auth::open_session(state, account).await?))
}

// Discord ------------------------------------------------------------------------------

fn discord_redirect(state: &AppState) -> String {
    format!("{}/api/auth/discord/callback", state.config.base_url)
}

/// `purpose`: sign_in, connect (needs `account_id`) or install.
pub async fn begin_discord(state: &AppState, purpose: &str, account_id: Option<Uuid>, return_path: &str) -> ApiResult<Started> {
    let (oauth_state, browser) = begin(state, "discord", purpose, account_id, return_path).await?;
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
