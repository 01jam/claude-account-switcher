//! Signing an account in from the app itself, without Claude Code's terminal.
//!
//! It is the flow `claude /login` runs — authorization code with PKCE, against
//! the same public client — in a window of the app's own rather than the
//! system browser. Two things come out of that:
//!
//! - the code never needs pasting: the window is ours, so it can watch for the
//!   redirect to the CLI's manual callback page and take the code off the URL
//!   before the page even loads;
//! - each account gets its own browser data. A new account starts from an
//!   empty one, so no claude.ai session already open elsewhere can answer for
//!   it; an account signing in again reopens its own, so the page usually
//!   only wants a click on "Authorize" — and when that session has lapsed too,
//!   it is the page that asks for the email.
//!
//! Whichever account the user ends up signing in as is what gets saved: an
//! email already stored updates that profile, any other becomes a new one.

use anyhow::{anyhow, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tokio::sync::oneshot;

use crate::claude;
use crate::i18n;
use crate::oauth;
use crate::store::{self, Meta, Profile};

/// The claude.ai flavour of the authorization page — the one a subscription
/// login goes through, as opposed to the Console's.
const AUTHORIZE_URL: &str = "https://claude.com/cai/oauth/authorize";

/// The CLI's "paste this code" page. Registered for the client, so the
/// authorization server will redirect to it; the window stops at the redirect
/// and the page itself is never shown.
const REDIRECT_URL: &str = "https://platform.claude.com/oauth/code/callback";

const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";

/// What `claude /login` asks for, so the tokens are the ones it would hold.
const SCOPES: &[&str] = &[
    "org:create_api_key",
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
    "user:mcp_servers",
    "user:file_upload",
    "user:plugins",
];

/// One login window at a time: two would race to save the same account.
const WINDOW_LABEL: &str = "login";

/// The window was closed before the sign-in finished. Not a failure — the user
/// changed their mind, and the window reports nothing.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", i18n::t("errors.login_cancelled"))
    }
}

impl std::error::Error for Cancelled {}

fn random_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow!("no randomness available: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn challenge_of(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn authorize_url(challenge: &str, state: &str, hint: Option<&str>) -> Result<Url> {
    let mut url = Url::parse(AUTHORIZE_URL)?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("code", "true")
            .append_pair("client_id", oauth::CLIENT_ID)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", REDIRECT_URL)
            .append_pair("scope", &SCOPES.join(" "))
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", state);
        if let Some(email) = hint {
            query.append_pair("login_hint", email);
        }
    }
    Ok(url)
}

/// Whether a navigation is the authorization server handing the code back.
fn is_redirect(url: &Url) -> bool {
    let target = Url::parse(REDIRECT_URL).expect("valid constant");
    url.scheme() == target.scheme()
        && url.host_str() == target.host_str()
        && url.path() == target.path()
}

/// What the redirect carried: the code, or the server's reason for not giving
/// one.
enum Answer {
    Code { code: String, state: String },
    Refused(String),
}

fn answer_of(url: &Url) -> Answer {
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let get = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    match (get("code"), get("state")) {
        (Some(code), Some(state)) => Answer::Code { code, state },
        _ => Answer::Refused(
            get("error_description")
                .or_else(|| get("error"))
                .unwrap_or_else(|| "no code in the redirect".into()),
        ),
    }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

type Reply = Arc<Mutex<Option<oneshot::Sender<Result<Answer, Cancelled>>>>>;

fn send(reply: &Reply, value: Result<Answer, Cancelled>) {
    if let Some(tx) = reply.lock().ok().and_then(|mut slot| slot.take()) {
        let _ = tx.send(value);
    }
}

/// Open the login window on `url` with `browser` as its data, and wait for the
/// redirect or for the window to be closed.
async fn ask(app: &AppHandle, url: Url, browser: &str) -> Result<Answer> {
    let (tx, rx) = oneshot::channel();
    let reply: Reply = Arc::new(Mutex::new(Some(tx)));

    let on_navigation = {
        let reply = reply.clone();
        let app = app.clone();
        move |url: &Url| {
            if !is_redirect(url) {
                return true;
            }
            send(&reply, Ok(answer_of(url)));
            // Off the navigation callback: closing from inside it would tear the
            // webview down while it is still deciding about this very load.
            let app_for_close = app.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(window) = app_for_close.get_webview_window(WINDOW_LABEL) {
                    let _ = window.close();
                }
            });
            false
        }
    };

    let builder = WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(url))
        .title(i18n::t("login.window_title"))
        .inner_size(480.0, 760.0)
        .min_inner_size(360.0, 480.0)
        .center()
        .on_navigation(on_navigation);

    #[cfg(not(target_os = "macos"))]
    let builder = builder.data_directory(store::browser_dir(browser)?);

    // WebKit on macOS has no data directories, only data stores named by 16
    // bytes — which is exactly what the folder name spells.
    #[cfg(target_os = "macos")]
    let builder = {
        let mut id = [0u8; 16];
        for (i, byte) in id.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&browser[i * 2..i * 2 + 2], 16).unwrap_or(0);
        }
        builder.data_store_identifier(id)
    };

    let window = builder.build()?;
    {
        let reply = reply.clone();
        window.on_window_event(move |event| {
            if let WindowEvent::Destroyed = event {
                send(&reply, Err(Cancelled));
            }
        });
    }
    let _ = window.set_focus();

    match rx.await {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err(cancelled)) => Err(cancelled.into()),
        Err(_) => Err(Cancelled.into()),
    }
}

// ---------------------------------------------------------------------------
// From the code to a saved account
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    scope: Option<String>,
}

async fn exchange(code: &str, state: &str, verifier: &str) -> Result<Tokens> {
    let client = reqwest::Client::builder()
        .timeout(oauth::REQUEST_TIMEOUT)
        .build()?;
    let response = client
        .post(oauth::TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&json!({
            "grant_type": "authorization_code",
            "code": code,
            "redirect_uri": REDIRECT_URL,
            "client_id": oauth::CLIENT_ID,
            "code_verifier": verifier,
            "state": state,
        }))
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let refusal: oauth::Refusal = response.json().await.unwrap_or_default();
        let mut message =
            i18n::t_args("errors.api_status", &[("status", &status.as_u16().to_string())]);
        if let Some(summary) = refusal.summary() {
            message = format!("{message} ({summary})");
        }
        return Err(anyhow!(i18n::t_args("errors.login_failed", &[("error", &message)])));
    }
    Ok(response.json().await?)
}

async fn fetch_profile(access_token: &str) -> Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(oauth::REQUEST_TIMEOUT)
        .build()?;
    let response = client
        .get(PROFILE_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Content-Type", "application/json")
        .header("anthropic-beta", oauth::BETA_HEADER)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!(i18n::t_args(
            "errors.api_status",
            &[("status", &status.as_u16().to_string())]
        )));
    }
    Ok(response.json().await?)
}

/// `claude_max` → `max`, as the CLI files it under `subscriptionType`.
fn subscription_of(profile: &Value) -> Option<&'static str> {
    match profile.pointer("/organization/organization_type")?.as_str()? {
        "claude_max" => Some("max"),
        "claude_pro" => Some("pro"),
        "claude_enterprise" => Some("enterprise"),
        "claude_team" => Some("team"),
        _ => None,
    }
}

/// The login file's document, in the shape Claude Code writes it.
fn credentials_of(tokens: &Tokens, profile: &Value) -> Value {
    let scopes: Vec<&str> = match &tokens.scope {
        Some(scope) => scope.split(' ').filter(|s| !s.is_empty()).collect(),
        None => SCOPES.to_vec(),
    };
    json!({
        "claudeAiOauth": {
            "accessToken": tokens.access_token,
            "refreshToken": tokens.refresh_token,
            "expiresAt": store::now_ms() + tokens.expires_in.unwrap_or(3600) * 1000,
            "scopes": scopes,
            "subscriptionType": subscription_of(profile),
            "rateLimitTier": profile.pointer("/organization/rate_limit_tier"),
        }
    })
}

/// The `oauthAccount` slice of `~/.claude.json`, as the CLI fills it in after
/// its own login. Only what the profile actually carries: an absent field is
/// one the CLI fetches for itself on its next start.
fn account_of(profile: &Value) -> Result<Value> {
    let account = profile.get("account").cloned().unwrap_or(Value::Null);
    let org = profile.get("organization").cloned().unwrap_or(Value::Null);
    let email = account
        .get("email")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!(i18n::t("errors.login_no_email")))?;

    let mut out = Map::new();
    let mut put = |key: &str, value: Option<&Value>| {
        if let Some(v) = value.filter(|v| !v.is_null()) {
            out.insert(key.to_string(), v.clone());
        }
    };
    put("accountUuid", account.get("uuid"));
    put("emailAddress", Some(&json!(email)));
    put("organizationUuid", org.get("uuid"));
    put("displayName", account.get("display_name"));
    put("fullName", account.get("full_name"));
    put("accountCreatedAt", account.get("created_at"));
    put("hasExtraUsageEnabled", org.get("has_extra_usage_enabled"));
    put("billingType", org.get("billing_type"));
    put("subscriptionCreatedAt", org.get("subscription_created_at"));
    put("ccOnboardingFlags", Some(org.get("cc_onboarding_flags").unwrap_or(&json!({}))));
    put("claudeCodeTrialEndsAt", org.get("claude_code_trial_ends_at"));
    put("claudeCodeTrialDurationDays", org.get("claude_code_trial_duration_days"));
    put("seatTier", org.get("seat_tier"));
    put("organizationName", org.get("name"));
    put("profileFetchedAt", Some(&json!(store::now_ms())));

    Ok(json!({ "oauthAccount": Value::Object(out) }))
}

/// File the new login under the profile with the same email, or a new one.
///
/// `browser` follows the account that actually signed in: a re-login of one
/// account that ended up as another hands that session to the other, since it
/// is the other's cookies that folder now holds.
fn save(target: Option<&str>, browser: &str, credentials: &Value, account: &Value) -> Result<Profile> {
    let email = claude::email_of(account);
    let existing = store::list()?
        .into_iter()
        .find(|p| p.email.is_some() && p.email == email);

    let id = match existing {
        Some(profile) => {
            let mut meta = store::load_meta(&profile.id)?;
            if let Some(old) = meta.browser.as_deref().filter(|old| *old != browser) {
                store::remove_browser(old);
            }
            meta.browser = Some(browser.to_string());
            meta.subscription = claude::subscription_of(credentials).or(meta.subscription);
            meta.expires_at = claude::expires_at_of(credentials);

            // The CLI's identity keys are kept as they were, the fresh
            // `oauthAccount` laid over them.
            let mut stored = store::load_account(&profile.id)?;
            if let (Some(stored), Some(fresh)) = (stored.as_object_mut(), account.as_object()) {
                for (key, value) in fresh {
                    stored.insert(key.clone(), value.clone());
                }
            }
            store::save(&profile.id, &meta, credentials, &stored)?;

            // The account Claude Code is running under right now: the login it
            // reads has to be the new one too, or the next sync would copy the
            // dead tokens straight back over these.
            if store::active()?.as_deref() == Some(profile.id.as_str()) {
                claude::write_credentials(credentials)?;
                claude::write_account_keys(&stored)?;
            }
            profile.id
        }
        None => {
            let label = email.clone().unwrap_or_else(|| i18n::t("accounts.default_label"));
            let id = store::unique_id(&label)?;
            let meta = Meta {
                label,
                email: email.clone(),
                subscription: claude::subscription_of(credentials),
                created_at: store::now_ms(),
                last_used: None,
                expires_at: claude::expires_at_of(credentials),
                five_hour_threshold: store::DEFAULT_THRESHOLD,
                seven_day_threshold: store::DEFAULT_THRESHOLD,
                order: None,
                browser: Some(browser.to_string()),
            };
            store::save(&id, &meta, credentials, account)?;
            id
        }
    };

    if let Some(target) = target.filter(|t| *t != id) {
        if let Ok(mut meta) = store::load_meta(target) {
            if meta.browser.as_deref() == Some(browser) {
                meta.browser = None;
                store::save_meta(target, &meta)?;
            }
        }
    }

    store::list()?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| anyhow!("profile disappeared after saving"))
}

/// Sign in through the app's own window and save whatever account comes out.
///
/// `target` is the account being signed in again, if any: its browser data is
/// reopened and its email suggested to the page. `None` is a new account, with
/// browser data nobody has used.
pub async fn run(app: &AppHandle, target: Option<&str>) -> Result<Profile> {
    if let Some(window) = app.get_webview_window(WINDOW_LABEL) {
        let _ = window.set_focus();
        return Err(anyhow!(i18n::t("errors.login_open")));
    }

    let meta = target.map(store::load_meta).transpose()?;
    let reused = meta.as_ref().and_then(|m| m.browser.clone());
    let browser = match &reused {
        Some(name) => name.clone(),
        None => store::new_browser_name()?,
    };
    let hint = meta.as_ref().and_then(|m| m.email.clone());

    let verifier = random_token()?;
    let state = random_token()?;
    let url = authorize_url(&challenge_of(&verifier), &state, hint.as_deref())?;

    let outcome = async {
        let (code, returned) = match ask(app, url, &browser).await? {
            Answer::Code { code, state } => (code, state),
            Answer::Refused(reason) => {
                return Err(anyhow!(i18n::t_args("errors.login_failed", &[("error", &reason)])))
            }
        };
        if returned != state {
            return Err(anyhow!(i18n::t_args(
                "errors.login_failed",
                &[("error", "state mismatch")]
            )));
        }

        let tokens = exchange(&code, &state, &verifier).await?;
        if tokens.refresh_token.is_none() {
            return Err(anyhow!(i18n::t("errors.no_refresh_token")));
        }
        let profile = fetch_profile(&tokens.access_token).await?;
        let credentials = credentials_of(&tokens, &profile);
        let account = account_of(&profile)?;

        // Under the renewal gate: a keeper pass that read this account's old
        // tokens before the save would otherwise write them back after it.
        let _turn = oauth::gate().lock().await;
        let target = target.map(str::to_string);
        let browser = browser.clone();
        tauri::async_runtime::spawn_blocking(move || {
            save(target.as_deref(), &browser, &credentials, &account)
        })
        .await?
    }
    .await;

    // A folder made for this attempt and never handed to anyone holds a
    // half-finished session that no button will ever reopen.
    if outcome.is_err() && reused.is_none() {
        store::remove_browser(&browser);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636, appendix B.
    #[test]
    fn the_challenge_is_the_rfc_one() {
        assert_eq!(
            challenge_of("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn only_the_callback_page_counts_as_the_redirect() {
        let hit = Url::parse("https://platform.claude.com/oauth/code/callback?code=a&state=b").unwrap();
        assert!(is_redirect(&hit));
        assert!(!is_redirect(&Url::parse("https://claude.ai/login").unwrap()));
        assert!(!is_redirect(
            &Url::parse("https://platform.claude.com/oauth/code/success").unwrap()
        ));
        match answer_of(&hit) {
            Answer::Code { code, state } => assert_eq!((code.as_str(), state.as_str()), ("a", "b")),
            Answer::Refused(_) => panic!("expected a code"),
        }
    }

    #[test]
    fn a_redirect_without_a_code_says_why() {
        let url = Url::parse(
            "https://platform.claude.com/oauth/code/callback?error=access_denied&error_description=denied",
        )
        .unwrap();
        match answer_of(&url) {
            Answer::Refused(reason) => assert_eq!(reason, "denied"),
            Answer::Code { .. } => panic!("expected a refusal"),
        }
    }

    #[test]
    fn the_account_slice_carries_the_email() {
        let profile = json!({
            "account": { "uuid": "u", "email": "a@b.c", "display_name": "A" },
            "organization": { "uuid": "o", "organization_type": "claude_max", "rate_limit_tier": "t" },
        });
        let account = account_of(&profile).unwrap();
        assert_eq!(account["oauthAccount"]["emailAddress"], "a@b.c");
        assert_eq!(account["oauthAccount"]["organizationUuid"], "o");
        assert_eq!(subscription_of(&profile), Some("max"));
        assert!(account_of(&json!({})).is_err());
    }
}
