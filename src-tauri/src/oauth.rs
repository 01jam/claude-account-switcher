//! Renewing the OAuth tokens, instead of waiting for Claude Code to do it.
//!
//! Claude Code owns the login *flow*, but not the renewal: the refresh token it
//! stored was issued to its own public client, so the same grant works from
//! here and produces exactly the credentials the CLI would have written itself.
//! Without this, an account nobody has run `claude` under simply rots — its
//! access token expires, its meters go empty, and the auto-switch has nothing to
//! judge it on.
//!
//! Two rules keep it safe beside a running session:
//!
//! - the *write* goes through the same lock the CLI takes (`claude::lock_writes`),
//!   and re-reads the file inside it, so whoever renewed first wins and the
//!   other stands down rather than overwriting;
//! - the live login is only renewed inside the window where the CLI would renew
//!   it anyway (`LIVE_MARGIN_MS`). A refresh rotates the refresh token, and a
//!   session holding the old one in memory re-reads the file before using it —
//!   but there is no reason to make it do that while its token is still good.
//!
//! A stored account has no such neighbour, so it is renewed as early as
//! `STORED_MARGIN_MS`.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::Mutex;

use crate::claude;
use crate::i18n;
use crate::store;

/// Claude Code's token endpoint and public client id, both as the shipped CLI
/// uses them. The client id is what the stored refresh token was issued to:
/// no other value can renew it.
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const BETA_HEADER: &str = "oauth-2025-04-20";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to wait for the login write lock. Claude Code holds it only for the
/// milliseconds a write takes, so failing to get it inside this means a session
/// is genuinely mid-write — and the keeper is coming back in seconds anyway.
const LOCK_WAIT: Duration = Duration::from_secs(3);

/// The live login is renewed only this close to expiry: the same window Claude
/// Code itself would refresh in, which is what makes a hot renewal a no-op for
/// the session rather than a surprise.
pub const LIVE_MARGIN_MS: u64 = 5 * 60 * 1000;

/// A stored account has no session keeping it alive, so it is renewed well
/// before it expires — early enough that its meters never go empty.
pub const STORED_MARGIN_MS: u64 = 30 * 60 * 1000;

/// Renew whatever the state of the token: what an explicit "renew this account"
/// means, and what a 401 justifies when `expiresAt` claimed otherwise.
pub const FORCE: u64 = u64::MAX;

/// The endpoint always sends `expires_in`; this is only so a reply without one
/// cannot leave behind a token that never looks due again.
const FALLBACK_LIFETIME_MS: u64 = 60 * 60 * 1000;

/// Scopes to ask for when the stored credentials carry none of their own.
const DEFAULT_SCOPES: &[&str] = &[
    "user:inference",
    "user:profile",
    "user:sessions:claude_code",
];

/// A refusal no retry can fix: the refresh token has been spent, rotated away
/// or revoked, and only a new `claude` login can replace it.
#[derive(Debug)]
pub struct Rejected;

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", i18n::t("errors.refresh_rejected"))
    }
}

impl std::error::Error for Rejected {}

// ---------------------------------------------------------------------------
// The credentials document
// ---------------------------------------------------------------------------

fn refresh_token_of(credentials: &Value) -> Option<String> {
    credentials
        .get("claudeAiOauth")?
        .get("refreshToken")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn scopes_of(credentials: &Value) -> Vec<String> {
    credentials
        .get("claudeAiOauth")
        .and_then(|o| o.get("scopes"))
        .and_then(|s| s.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .filter(|list| !list.is_empty())
        .unwrap_or_else(|| DEFAULT_SCOPES.iter().map(|s| s.to_string()).collect())
}

/// Whether this token is close enough to expiry to be worth renewing.
pub fn due(credentials: &Value, margin_ms: u64) -> bool {
    match claude::expires_at_of(credentials) {
        Some(at) => at <= store::now_ms().saturating_add(margin_ms),
        // Nothing recorded says nothing is wrong: rotating a token that may
        // well be fine is the more expensive mistake, so only an explicit
        // request goes ahead.
        None => margin_ms == FORCE,
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    scope: Option<String>,
}

/// The renewed token folded back into the credentials document, leaving every
/// key we do not own (`subscriptionType`, `rateLimitTier`, anything a newer CLI
/// adds) exactly as it was.
fn merged(credentials: &Value, token: &TokenResponse, previous_refresh: &str) -> Value {
    let mut out = credentials.clone();
    if !out.is_object() {
        out = json!({});
    }
    let oauth = out
        .as_object_mut()
        .expect("object")
        .entry("claudeAiOauth")
        .or_insert_with(|| json!({}));
    if !oauth.is_object() {
        *oauth = json!({});
    }
    let oauth = oauth.as_object_mut().expect("object");

    oauth.insert("accessToken".into(), json!(token.access_token));
    oauth.insert(
        "refreshToken".into(),
        json!(token
            .refresh_token
            .clone()
            .unwrap_or_else(|| previous_refresh.to_string())),
    );
    oauth.insert(
        "expiresAt".into(),
        json!(store::now_ms() + token.expires_in.map(|s| s * 1000).unwrap_or(FALLBACK_LIFETIME_MS)),
    );
    if let Some(scope) = &token.scope {
        let scopes: Vec<&str> = scope.split(' ').filter(|s| !s.is_empty()).collect();
        if !scopes.is_empty() {
            oauth.insert("scopes".into(), json!(scopes));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The call
// ---------------------------------------------------------------------------

async fn request(refresh_token: &str, scopes: &[String]) -> Result<TokenResponse> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;

    let response = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("anthropic-beta", BETA_HEADER)
        .json(&json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": CLIENT_ID,
            "scope": scopes.join(" "),
        }))
        .send()
        .await?;

    let status = response.status();
    // 400 is what a spent or revoked refresh token earns, 401 a rejected
    // client: neither improves by asking again.
    if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(Rejected.into());
    }
    if !status.is_success() {
        return Err(anyhow!(i18n::t_args(
            "errors.api_status",
            &[("status", &status.as_u16().to_string())]
        )));
    }

    Ok(response.json().await?)
}

// ---------------------------------------------------------------------------
// Where the result goes
// ---------------------------------------------------------------------------

/// Renew the live login, the one Claude Code reads.
///
/// `Ok(None)` means there was nothing to do — not due, no login, or a session
/// renewed it first, which is just as good an outcome as doing it ourselves.
async fn renew_live(margin_ms: u64) -> Result<Option<Value>> {
    let Some(live) = claude::read_credentials()? else {
        return Ok(None);
    };
    if !due(&live, margin_ms) {
        return Ok(None);
    }
    let Some(previous) = refresh_token_of(&live) else {
        return Err(anyhow!(i18n::t("errors.no_refresh_token")));
    };

    // Outside the lock, exactly as Claude Code does it: the network call is the
    // slow part, and holding a lock across it would stall a session that only
    // wants to write.
    let token = request(&previous, &scopes_of(&live)).await?;

    let written = tauri::async_runtime::spawn_blocking(move || {
        claude::update_credentials(LOCK_WAIT, |current| {
            let Some(current) = current else {
                // The login vanished while we were asking — a logout, most
                // likely. Writing it back would sign the user in again.
                return Ok(None);
            };
            // Someone renewed while we were on the network. Their tokens are on
            // disk and ours are already stale: leave theirs alone.
            if refresh_token_of(&current).as_deref() != Some(previous.as_str()) {
                return Ok(None);
            }
            Ok(Some(merged(&current, &token, &previous)))
        })
    })
    .await??;

    if written.is_some() {
        // The profile keeps its own copy, and a switch restores from it: an
        // un-mirrored renewal would be undone by the next switch back.
        crate::actions::sync_active()?;
    }
    Ok(written)
}

/// Renew the copy of an account this app is holding for later. Nothing else
/// reads that file, so there is no lock and no one to race.
async fn renew_stored(id: &str, margin_ms: u64) -> Result<Option<Value>> {
    let stored = store::load_credentials(id)?;
    if !due(&stored, margin_ms) {
        return Ok(None);
    }
    let Some(previous) = refresh_token_of(&stored) else {
        return Err(anyhow!(i18n::t("errors.no_refresh_token")));
    };

    let token = request(&previous, &scopes_of(&stored)).await?;
    let renewed = merged(&stored, &token, &previous);

    store::save_credentials(id, &renewed)?;
    if let Ok(mut meta) = store::load_meta(id) {
        meta.expires_at = claude::expires_at_of(&renewed);
        store::save_meta(id, &meta)?;
    }
    Ok(Some(renewed))
}

/// One renewal at a time, app-wide.
///
/// A refresh rotates the token it was made with, so two renewals racing over
/// the same account leave one of them holding a refresh token the server has
/// already retired — and reporting a dead login that is in fact perfectly
/// healthy. Whoever waits here re-reads the credentials afterwards and finds
/// nothing left to do, which is the correct answer.
fn gate() -> &'static Mutex<()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(()))
}

/// Renew one account wherever its token actually lives, and hand back the
/// credentials that are now on disk for it.
pub async fn renew(id: &str, active: Option<&str>, margin_ms: u64) -> Result<Option<Value>> {
    let _turn = gate().lock().await;
    if active == Some(id) {
        renew_live(margin_ms).await
    } else {
        renew_stored(id, margin_ms).await
    }
}

/// What one account's renewal came to, in the terms the UI reports.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// A new token was written.
    Renewed,
    /// Nothing was due — the token still has time on it.
    Fresh,
    /// A session held the login lock; the keeper will try again shortly.
    Deferred,
    /// The renewal was refused or failed; `error` says how.
    Failed,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub id: String,
    pub label: String,
    pub status: Status,
    pub error: Option<String>,
}

/// Renew every saved account whose token is due, one at a time.
///
/// This is the whole keep-alive: called on a short timer it is a handful of
/// file reads and no traffic at all until something is actually near expiry —
/// and a renewal a busy session deferred lands on the next pass, seconds after
/// the session lets go, rather than at the next usage poll.
pub async fn renew_due(margin_live: u64, margin_stored: u64) -> Vec<Outcome> {
    let Ok(profiles) = store::list() else {
        return Vec::new();
    };
    let active = store::active().unwrap_or(None);

    let mut out = Vec::new();
    for profile in profiles {
        let margin = if active.as_deref() == Some(profile.id.as_str()) {
            margin_live
        } else {
            margin_stored
        };
        let (status, error) = match renew(&profile.id, active.as_deref(), margin).await {
            Ok(Some(_)) => (Status::Renewed, None),
            Ok(None) => (Status::Fresh, None),
            Err(e) if e.downcast_ref::<claude::Busy>().is_some() => (Status::Deferred, None),
            Err(e) => (Status::Failed, Some(format!("{e:#}"))),
        };
        out.push(Outcome {
            id: profile.id,
            label: profile.label,
            status,
            error,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(expires_at: Option<u64>) -> Value {
        let mut oauth = json!({
            "accessToken": "old-access",
            "refreshToken": "old-refresh",
            "scopes": ["user:inference", "user:profile"],
            "subscriptionType": "pro",
            "rateLimitTier": "default_claude_pro",
        });
        if let Some(at) = expires_at {
            oauth["expiresAt"] = json!(at);
        }
        json!({ "claudeAiOauth": oauth })
    }

    fn response(refresh: Option<&str>) -> TokenResponse {
        TokenResponse {
            access_token: "new-access".into(),
            refresh_token: refresh.map(str::to_string),
            expires_in: Some(3600),
            scope: Some("user:inference user:profile".into()),
        }
    }

    #[test]
    fn due_follows_the_margin() {
        let in_ten_minutes = store::now_ms() + 10 * 60 * 1000;
        let creds = credentials(Some(in_ten_minutes));

        assert!(!due(&creds, LIVE_MARGIN_MS), "five minutes of margin is not yet ten");
        assert!(due(&creds, STORED_MARGIN_MS), "thirty is");
        assert!(due(&credentials(Some(store::now_ms() - 1)), 0), "expired is always due");
    }

    /// An account with no recorded expiry is left alone by the keeper — but an
    /// explicit request still goes through.
    #[test]
    fn an_unknown_expiry_is_only_renewed_on_request() {
        let creds = credentials(None);
        assert!(!due(&creds, STORED_MARGIN_MS));
        assert!(due(&creds, FORCE));
    }

    #[test]
    fn merging_keeps_the_fields_we_do_not_own() {
        let before = credentials(Some(0));
        let after = merged(&before, &response(Some("new-refresh")), "old-refresh");
        let oauth = &after["claudeAiOauth"];

        assert_eq!(oauth["accessToken"], "new-access");
        assert_eq!(oauth["refreshToken"], "new-refresh");
        assert_eq!(oauth["subscriptionType"], "pro");
        assert_eq!(oauth["rateLimitTier"], "default_claude_pro");
        assert!(oauth["expiresAt"].as_u64().unwrap() > store::now_ms());
    }

    /// The endpoint only returns a new refresh token when it rotates one; a
    /// reply without it must not blank the one we have.
    #[test]
    fn a_reply_without_a_refresh_token_keeps_the_old_one() {
        let after = merged(&credentials(Some(0)), &response(None), "old-refresh");
        assert_eq!(after["claudeAiOauth"]["refreshToken"], "old-refresh");
    }

    #[test]
    fn scopes_fall_back_only_when_there_are_none() {
        assert_eq!(
            scopes_of(&credentials(Some(0))),
            vec!["user:inference".to_string(), "user:profile".to_string()]
        );
        assert_eq!(scopes_of(&json!({})).len(), DEFAULT_SCOPES.len());
    }
}
