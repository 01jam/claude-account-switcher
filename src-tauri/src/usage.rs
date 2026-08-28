//! Live plan usage, read from the endpoint Claude Code itself polls for its
//! `/usage` view: `GET /api/oauth/usage` with the account's OAuth bearer token.
//!
//! This endpoint is not part of the public API and its shape may change without
//! notice. Everything here is therefore optional and failure-tolerant: when a
//! field is missing or the call fails, the UI shows "no data" rather than
//! breaking, and the auto-switch simply does not fire.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use crate::i18n;
use crate::store;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// How close to its threshold an account has to get before the tray warns about
/// it, in percentage points.
pub const WARN_MARGIN: f64 = 5.0;

/// How long a fetched snapshot is served from cache. The underlying numbers move
/// slowly, and this keeps a busy UI from hammering the endpoint.
const CACHE_TTL_MS: u64 = 300_000;

/// After a 429 the endpoint is left alone for this long. Usage is a nicety —
/// never a reason to keep pushing against a rate limit.
const COOLDOWN_MS: u64 = 600_000;

/// One rolling limit window as the UI needs it.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    /// Percentage of the window consumed, 0-100.
    pub utilization: f64,
    /// ISO-8601 instant at which this window resets, straight from the API.
    pub resets_at: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub fetched_at: u64,
}

impl Usage {
    /// The worst of the two windows against the given thresholds, if either has
    /// data. `None` means "nothing to judge on".
    pub fn hits(&self, five_hour_threshold: f64, seven_day_threshold: f64) -> Option<Hit> {
        let candidates = [
            (self.five_hour.as_ref(), five_hour_threshold, Kind::FiveHour),
            (self.seven_day.as_ref(), seven_day_threshold, Kind::SevenDay),
        ];
        candidates
            .into_iter()
            .filter_map(|(window, threshold, kind)| {
                let w = window?;
                (w.utilization >= threshold).then_some(Hit {
                    kind,
                    utilization: w.utilization,
                    threshold,
                })
            })
            .next()
    }

    /// Like `hits`, but fires `margin` points early — "about to run out" rather
    /// than "out". The floor keeps a very low threshold from warning always.
    pub fn approaching(&self, five_hour: f64, seven_day: f64, margin: f64) -> Option<Hit> {
        self.hits(
            (five_hour - margin).max(1.0),
            (seven_day - margin).max(1.0),
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    FiveHour,
    SevenDay,
}

impl Kind {
    pub fn label(self) -> String {
        match self {
            Kind::FiveHour => i18n::t("usage.five_hour"),
            Kind::SevenDay => i18n::t("usage.seven_day"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub kind: Kind,
    pub utilization: f64,
    pub threshold: f64,
}

/// Usage for one saved account, as handed to the UI. Exactly one of `usage` and
/// `error` is set.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProfileUsage {
    pub id: String,
    pub usage: Option<Usage>,
    pub error: Option<String>,
    /// Epoch ms of the next allowed request while a rate-limit cooldown runs.
    pub retry_at: Option<u64>,
}

// ---------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------

/// Distinguishable so a 429 can pause every account at once, not just the one
/// whose request happened to be refused.
///
/// `retry_after` carries the header's value when the server sends a usable one.
/// In practice this endpoint answers `retry-after: 0` and a body with no
/// deadline in it, so the fallback cooldown is what normally applies.
#[derive(Debug)]
struct RateLimited {
    retry_after: Option<Duration>,
}

impl RateLimited {
    fn cooldown(&self) -> Duration {
        self.retry_after
            .filter(|d| !d.is_zero())
            .unwrap_or(Duration::from_millis(COOLDOWN_MS))
    }
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", i18n::t("errors.rate_limited"))
    }
}

impl std::error::Error for RateLimited {}

/// `Retry-After` is either a count of seconds or an HTTP date; only the first
/// form is worth handling here, and even that is rarely populated.
fn retry_after_of(response: &reqwest::Response) -> Option<Duration> {
    let raw = response.headers().get(reqwest::header::RETRY_AFTER)?;
    let seconds: u64 = raw.to_str().ok()?.trim().parse().ok()?;
    Some(Duration::from_secs(seconds.min(3600)))
}

#[derive(Deserialize)]
struct ApiWindow {
    utilization: Option<f64>,
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct ApiUsage {
    five_hour: Option<ApiWindow>,
    seven_day: Option<ApiWindow>,
}

fn convert(window: Option<ApiWindow>) -> Option<Window> {
    let w = window?;
    Some(Window {
        utilization: w.utilization?.clamp(0.0, 100.0),
        resets_at: w.resets_at,
    })
}

pub fn access_token_of(credentials: &Value) -> Option<String> {
    credentials
        .get("claudeAiOauth")?
        .get("accessToken")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

async fn fetch(token: &str) -> Result<Usage> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()?;

    let response = client
        .get(USAGE_URL)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await?;

    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(anyhow!(i18n::t("errors.invalid_token")));
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(RateLimited {
            retry_after: retry_after_of(&response),
        }
        .into());
    }
    if !status.is_success() {
        return Err(anyhow!(i18n::t_args(
            "errors.api_status",
            &[("status", &status.as_u16().to_string())]
        )));
    }

    let api: ApiUsage = response.json().await?;
    Ok(Usage {
        five_hour: convert(api.five_hour),
        seven_day: convert(api.seven_day),
        fetched_at: store::now_ms(),
    })
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Cache {
    entries: Mutex<HashMap<String, Usage>>,
    /// Epoch ms before which no request may be made, set after a 429.
    cooldown_until: Mutex<u64>,
}

/// The cache as it is written to disk.
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Persisted {
    entries: HashMap<String, Usage>,
    cooldown_until: u64,
}

fn cache_path() -> Option<PathBuf> {
    store::root().ok().map(|dir| dir.join("usage.json"))
}

impl Cache {
    /// Start from what the last run left behind.
    ///
    /// Holding this only in memory meant every restart began blind and had to
    /// re-ask for every account — which is the surest way to earn the 429 that
    /// then leaves the auto-switch with nothing to judge on for ten minutes.
    /// The cooldown is restored too, for the same reason.
    pub fn load() -> Cache {
        let Some(path) = cache_path() else {
            return Cache::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Cache::default();
        };
        let saved: Persisted = serde_json::from_str(&text).unwrap_or_default();
        Cache {
            entries: Mutex::new(saved.entries),
            cooldown_until: Mutex::new(saved.cooldown_until),
        }
    }

    /// Best effort: a snapshot that fails to save costs a refetch, nothing more.
    fn save(&self) {
        let (Some(path), Ok(entries), Ok(cooldown_until)) =
            (cache_path(), self.entries.lock(), self.cooldown_until.lock())
        else {
            return;
        };
        let saved = Persisted {
            entries: entries.clone(),
            cooldown_until: *cooldown_until,
        };
        if let Ok(json) = serde_json::to_vec_pretty(&saved) {
            let _ = std::fs::write(path, json);
        }
    }
    fn get_fresh(&self, id: &str) -> Option<Usage> {
        let map = self.entries.lock().ok()?;
        let usage = map.get(id)?;
        (store::now_ms().saturating_sub(usage.fetched_at) < CACHE_TTL_MS).then(|| usage.clone())
    }

    fn get_any(&self, id: &str) -> Option<Usage> {
        self.entries.lock().ok()?.get(id).cloned()
    }

    fn put(&self, id: &str, usage: &Usage) {
        if let Ok(mut map) = self.entries.lock() {
            map.insert(id.to_string(), usage.clone());
        }
        self.save();
    }

    fn in_cooldown(&self) -> bool {
        self.retry_at().is_some()
    }

    /// When the next request will be allowed, if a cooldown is running.
    pub fn retry_at(&self) -> Option<u64> {
        let until = *self.cooldown_until.lock().ok()?;
        (until > store::now_ms()).then_some(until)
    }

    fn start_cooldown(&self, for_duration: Duration) {
        if let Ok(mut until) = self.cooldown_until.lock() {
            *until = store::now_ms() + for_duration.as_millis() as u64;
        }
        self.save();
    }

    pub fn forget(&self, id: &str) {
        if let Ok(mut map) = self.entries.lock() {
            map.remove(id);
        }
        self.save();
    }

    /// Everything currently known, stale entries included. The tray menu is
    /// rebuilt synchronously and must never wait on the network, so it shows the
    /// last known numbers rather than none at all.
    pub fn snapshot(&self) -> HashMap<String, Usage> {
        self.entries.lock().map(|m| m.clone()).unwrap_or_default()
    }
}

/// Credentials to read usage with: the live ones for the active account (Claude
/// Code keeps them refreshed), the stored copy for everyone else.
fn credentials_for(id: &str, active: Option<&str>) -> Result<Value> {
    if active == Some(id) {
        if let Some(live) = crate::claude::read_credentials()? {
            return Ok(live);
        }
    }
    store::load_credentials(id)
}

/// Usage for one account, cached. `force` bypasses the cache.
pub async fn for_profile(cache: &Cache, id: &str, force: bool) -> Result<Usage> {
    if !force {
        if let Some(hit) = cache.get_fresh(id) {
            return Ok(hit);
        }
    }

    // While rate-limited, stale numbers beat no numbers and beat another
    // refused request.
    if cache.in_cooldown() {
        return cache
            .get_any(id)
            .ok_or_else(|| anyhow!(i18n::t("errors.rate_limited")));
    }

    let active = store::active()?;
    let credentials = credentials_for(id, active.as_deref())?;
    let token = access_token_of(&credentials)
        .ok_or_else(|| anyhow!(i18n::t("errors.no_token")))?;

    match fetch(&token).await {
        Ok(usage) => {
            cache.put(id, &usage);
            Ok(usage)
        }
        Err(e) => {
            if let Some(limited) = e.downcast_ref::<RateLimited>() {
                cache.start_cooldown(limited.cooldown());
                if let Some(stale) = cache.get_any(id) {
                    return Ok(stale);
                }
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed to the fields we read, but with the null-heavy shape the real
    /// endpoint returns — most windows are null for a given plan.
    const SAMPLE: &str = r#"{
        "five_hour": {"utilization": 3.0, "resets_at": "2026-08-28T16:59:59.832047+00:00"},
        "seven_day": {"utilization": 29.0, "resets_at": "2026-08-30T22:59:59.832072+00:00"},
        "seven_day_opus": null,
        "nimbus_quill": {"utilization": 0.0, "resets_at": null},
        "extra_usage": {"is_enabled": true, "used_credits": 1308.0}
    }"#;

    fn parse(json: &str) -> Usage {
        let api: ApiUsage = serde_json::from_str(json).expect("parses");
        Usage {
            five_hour: convert(api.five_hour),
            seven_day: convert(api.seven_day),
            fetched_at: 0,
        }
    }

    #[test]
    fn reads_both_windows_and_ignores_unknown_fields() {
        let usage = parse(SAMPLE);
        let five = usage.five_hour.expect("five_hour present");
        assert_eq!(five.utilization, 3.0);
        assert_eq!(
            five.resets_at.as_deref(),
            Some("2026-08-28T16:59:59.832047+00:00")
        );
        assert_eq!(usage.seven_day.expect("seven_day present").utilization, 29.0);
    }

    #[test]
    fn a_null_window_is_simply_absent() {
        let usage = parse(r#"{"five_hour": null, "seven_day": {"utilization": 10.0}}"#);
        assert!(usage.five_hour.is_none());
        assert!(usage.seven_day.is_some());
    }

    /// A window present but without a number is not a zero — treating it as one
    /// would show an empty bar for an account we know nothing about.
    #[test]
    fn a_window_without_utilization_is_absent() {
        let usage = parse(r#"{"five_hour": {"resets_at": "2026-01-01T00:00:00Z"}}"#);
        assert!(usage.five_hour.is_none());
    }

    #[test]
    fn hits_fire_at_or_above_the_threshold() {
        let usage = parse(SAMPLE); // 3% and 29%

        assert!(usage.hits(100.0, 100.0).is_none());

        let hit = usage.hits(100.0, 29.0).expect("seven-day hit at exactly 29");
        assert_eq!(hit.kind, Kind::SevenDay);
        assert_eq!(hit.utilization, 29.0);

        // The five-hour window is checked first when both are over.
        assert_eq!(usage.hits(1.0, 1.0).unwrap().kind, Kind::FiveHour);
    }

    #[test]
    fn approaching_warns_before_the_threshold_is_reached() {
        let usage = parse(SAMPLE); // 3% and 29%

        // 29% is not yet at 32%, but it is within the 5-point margin.
        assert!(usage.hits(100.0, 32.0).is_none());
        assert_eq!(
            usage
                .approaching(100.0, 32.0, WARN_MARGIN)
                .expect("warns early")
                .kind,
            Kind::SevenDay
        );

        // Still silent when there is real room left.
        assert!(usage.approaching(100.0, 40.0, WARN_MARGIN).is_none());
    }

    /// With a threshold under the margin the warning would sit at or below zero
    /// and fire for every account; the floor prevents that.
    #[test]
    fn a_tiny_threshold_does_not_warn_on_an_idle_account() {
        let idle = parse(r#"{"five_hour": {"utilization": 0.0}}"#);
        assert!(idle.approaching(2.0, 2.0, WARN_MARGIN).is_none());
    }

    #[test]
    fn a_window_with_no_data_never_triggers_a_switch() {
        let usage = parse(r#"{"five_hour": null, "seven_day": null}"#);
        assert!(usage.hits(1.0, 1.0).is_none());
    }
}

/// Usage for every saved account, one entry each, errors included.
pub async fn for_all(cache: &Cache, force: bool) -> Result<Vec<ProfileUsage>> {
    let ids: Vec<String> = store::list()?.into_iter().map(|p| p.id).collect();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let entry = match for_profile(cache, &id, force).await {
            Ok(usage) => ProfileUsage {
                id,
                usage: Some(usage),
                error: None,
                retry_at: None,
            },
            Err(e) => ProfileUsage {
                id,
                usage: None,
                error: Some(format!("{e:#}")),
                // Set for every account: the cooldown is global, so this is
                // when the numbers can come back.
                retry_at: cache.retry_at(),
            },
        };
        out.push(entry);
    }
    Ok(out)
}
