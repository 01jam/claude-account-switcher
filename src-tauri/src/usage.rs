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
use crate::oauth;
use crate::pace;
use crate::store;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// How close to its threshold an account has to get before the tray warns about
/// it, in percentage points.
pub const WARN_MARGIN: f64 = 5.0;

/// The unit the weekly window's remaining time is measured in, and the length
/// of the window itself.
const DAY_MS: f64 = 86_400_000.0;
const WEEK_DAYS: f64 = 7.0;

/// How long a fetched snapshot is served from cache.
///
/// Just under the cadence floor, and that shortfall is deliberate. The budget
/// is spent by whoever asks, so a window opening, a tray rebuild and a switch
/// inside one interval have to cost one request between them rather than three
/// — that is what the TTL is for. But matching the floor exactly is the bug
/// this app already shipped once: a scheduled fetch landing a moment before an
/// entry ages out is handed the old copy, timestamp and all, and the account
/// waits another full interval. Half a minute of clearance and a due account is
/// always past it. See `pace` for where the floor comes from.
const CACHE_TTL_MS: u64 = pace::MIN_INTERVAL.as_millis() as u64 - 30_000;

/// How old a reading has to be before the card admits to it. Several polls'
/// worth, deliberately: the caption is for numbers the endpoint would not give
/// again — a cooldown, a token that cannot be renewed — and at one poll a minute
/// a single missed round is not worth putting on screen.
const STALE_AFTER_MS: u64 = 300_000;

/// How old a reading may be and still be worth acting on. Well past the TTL,
/// because a cooldown is allowed to keep numbers on screen — but a five-hour
/// window that has since reset still reads as full in the cache, and rotating
/// accounts on that is a switch the user never needed.
pub const DECISION_MAX_AGE_MS: u64 = 900_000;

/// After a 429 the endpoint is left alone for this long. Usage is a nicety —
/// never a reason to keep pushing against a rate limit.
const COOLDOWN_MS: u64 = 600_000;

/// The longest the app will sit blind, whatever `Retry-After` asks for.
///
/// The endpoint answers refusals with a blanket `retry-after: 3600` and is then
/// perfectly willing twenty minutes later: the hour is a stock number, not a
/// deadline it holds itself to. Past `DECISION_MAX_AGE_MS` the numbers are no
/// longer good enough to act on anyway, so that is exactly as long as it is
/// worth waiting before spending one request to find out.
const MAX_COOLDOWN_MS: u64 = DECISION_MAX_AGE_MS;

/// How often an explicit Refresh may push through a cooldown. Enough that
/// holding the button down cannot become a stream of refused requests.
const OVERRIDE_INTERVAL_MS: u64 = 180_000;

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
    /// Fresh enough to move accounts on. Displaying an old reading is fine —
    /// the card says how old it is — but deciding with one is not.
    pub fn is_actionable(&self) -> bool {
        store::now_ms().saturating_sub(self.fetched_at) <= DECISION_MAX_AGE_MS
    }

    /// Old enough that showing the number bare would be a small lie. One
    /// definition, used by the cards and the tray menu alike: a percentage
    /// that says nothing about its own age in one place and admits to it in
    /// the other is how someone ends up trusting the wrong one.
    pub fn is_stale(&self) -> bool {
        self.age_ms() >= STALE_AFTER_MS
    }

    pub fn age_ms(&self) -> u64 {
        store::now_ms().saturating_sub(self.fetched_at)
    }

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

    /// The window nearest its own threshold — the one that decides when this
    /// account is done, and so the one the cadence watches for movement.
    /// Returns what it reads and the threshold it is measured against.
    pub fn binding(&self, five_hour: f64, seven_day: f64) -> Option<(f64, f64)> {
        [
            (self.five_hour.as_ref(), five_hour),
            (self.seven_day.as_ref(), seven_day),
        ]
        .into_iter()
        .filter_map(|(window, threshold)| Some((window?.utilization, threshold)))
        .max_by(|a, b| {
            (a.0 - a.1)
                .partial_cmp(&(b.0 - b.1))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    }

    /// Like `hits`, but fires `margin` points early — "about to run out" rather
    /// than "out". The floor keeps a very low threshold from warning always.
    pub fn approaching(&self, five_hour: f64, seven_day: f64, margin: f64) -> Option<Hit> {
        self.hits(
            (five_hour - margin).max(1.0),
            (seven_day - margin).max(1.0),
        )
    }

    /// The weekly room left, spread over the days that window still has to
    /// run: the percentage points a day this account can afford between now
    /// and its reset. Higher is freer, and it is what the launch-time choice
    /// ranks accounts on.
    ///
    /// Dividing is what makes the two halves of the question one number — 20%
    /// left with six days to go is a tighter week than 70% left the day before
    /// a reset. A window resetting inside the day is floored at one day: below
    /// that the ratio runs away, and "it resets in ten minutes" is no reason to
    /// start on an account with nothing left in it right now. A window that
    /// names no reset is measured against the whole seven, which is the most
    /// cautious reading of a date we do not have.
    pub fn weekly_room_per_day(&self, now: u64) -> Option<f64> {
        let week = self.seven_day.as_ref()?;
        let days = week
            .days_to_reset(now)
            .unwrap_or(WEEK_DAYS)
            .clamp(1.0, WEEK_DAYS);
        Some((100.0 - week.utilization).max(0.0) / days)
    }
}

impl Window {
    /// How many days until this window resets, when the endpoint says so. A
    /// fraction rather than a count: the caller divides by it, it is not for
    /// printing.
    pub fn days_to_reset(&self, now: u64) -> Option<f64> {
        let at = epoch_ms(self.resets_at.as_deref()?)?;
        Some(((at - now as i64) as f64 / DAY_MS).max(0.0))
    }
}

/// Epoch milliseconds for the RFC-3339 instant this endpoint dates its resets
/// with: `2026-08-30T22:59:59.832072+00:00`, and sometimes `Z` in place of the
/// offset. Hand-rolled because that is the only date this app ever reads, and a
/// calendar library for one field is a dependency kept for its own sake.
fn epoch_ms(iso: &str) -> Option<i64> {
    let (date, rest) = iso.split_once(['T', 't', ' '])?;
    let mut fields = date.split('-');
    let year: i64 = fields.next()?.parse().ok()?;
    let month: u32 = fields.next()?.parse().ok()?;
    let day: u32 = fields.next()?.parse().ok()?;

    // What follows the seconds is `Z`, an offset, or nothing at all — and a
    // missing offset means UTC here, which is what this endpoint sends anyway.
    let (clock, offset) = match rest.strip_suffix(['Z', 'z']) {
        Some(head) => (head, 0),
        None => match rest.rfind(['+', '-']) {
            Some(at) => (&rest[..at], offset_ms(&rest[at..])?),
            None => (rest, 0),
        },
    };

    let mut parts = clock.split(':');
    let hours: i64 = parts.next()?.parse().ok()?;
    let minutes: i64 = parts.next()?.parse().ok()?;
    let field = parts.next().unwrap_or("0");
    let (seconds, millis) = match field.split_once('.') {
        // However many digits the fraction carries, only the first three are
        // milliseconds; this endpoint sends six.
        Some((whole, fraction)) => {
            let mut digits: String = fraction.chars().take(3).collect();
            while digits.len() < 3 {
                digits.push('0');
            }
            (whole.parse::<i64>().ok()?, digits.parse::<i64>().ok()?)
        }
        None => (field.parse::<i64>().ok()?, 0),
    };

    let seconds = days_from_civil(year, month, day) * 86_400 + hours * 3_600 + minutes * 60 + seconds;
    Some(seconds * 1_000 + millis - offset)
}

/// `+02:00`, `-0500` and `+00` are all shapes RFC 3339 and its neighbours allow.
fn offset_ms(tail: &str) -> Option<i64> {
    let sign = if tail.starts_with('-') { -1 } else { 1 };
    let body = tail.get(1..)?;
    let (hours, minutes) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        None if body.len() == 4 => (body[..2].parse().ok()?, body[2..].parse().ok()?),
        None => (body.parse::<i64>().ok()?, 0),
    };
    Some(sign * (hours * 3_600 + minutes * 60) * 1_000)
}

/// Days between the epoch and a civil date, by Howard Hinnant's closed form —
/// the same one the C++ standard's calendar is built on.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    // March-based years, so that the leap day lands at the end and the month
    // lengths fall into the pattern `(153 * m + 2) / 5` counts off.
    let year = year - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i64;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
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
    /// These numbers are past their TTL and were kept only because the endpoint
    /// could not be asked again. Without saying so the window looks freshly
    /// loaded while showing hours-old figures — which is exactly how a spent
    /// account can appear to have room left.
    pub stale: bool,
}

// ---------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------

/// Distinguishable so a refusal pauses the account it was about.
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
            .min(Duration::from_millis(MAX_COOLDOWN_MS))
    }
}

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", i18n::t("errors.rate_limited"))
    }
}

impl std::error::Error for RateLimited {}

/// The token was refused. Distinguishable because it is the one usage failure
/// this app can actually do something about: renew and ask again.
#[derive(Debug)]
struct Unauthorized;

impl std::fmt::Display for Unauthorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", i18n::t("errors.invalid_token"))
    }
}

impl std::error::Error for Unauthorized {}

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
        return Err(Unauthorized.into());
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
    /// Where this cache persists itself, or `None` for one that only lives in
    /// memory. A cache with nowhere to write is what the tests use: the file is
    /// a real one in the user's config directory, and a test has no business
    /// leaving "spent" and "one" in it.
    path: Option<PathBuf>,
    entries: Mutex<HashMap<String, Usage>>,
    /// Epoch ms before which each account may not be asked again, set from its
    /// own 429.
    ///
    /// Per account rather than app-wide, because the refusal is about the
    /// account: the endpoint answers for the token it was handed, and one
    /// account sitting at its own limit has no business blanking the meters of
    /// the others — which is precisely the account the user is about to switch
    /// to. The request rate is bounded by the cache TTL and the poll interval,
    /// not by pausing everything at the first refusal.
    cooldowns: Mutex<HashMap<String, u64>>,
    /// When an explicit Refresh last pushed through a cooldown.
    last_override: Mutex<u64>,
    /// When each account is next due, and what the last plan decided. Persisted
    /// so that a restart does not put every account back in the queue at once —
    /// which is a burst, and a burst is what saturates the trailing hour.
    schedules: Mutex<HashMap<String, Schedule>>,
}

/// One account's place in the queue, as `pace` last worked it out.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub next_at: u64,
    pub interval_s: u64,
    /// What the binding window read last time, for spotting movement.
    pub binding: Option<f64>,
    /// When this account was last refused, remembered past the cooldown itself
    /// because the hour it takes to age out is longer than the cooldown is.
    pub last_refusal: Option<u64>,
}

/// The cache as it is written to disk.
#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Persisted {
    entries: HashMap<String, Usage>,
    #[serde(default)]
    cooldowns: HashMap<String, u64>,
    #[serde(default)]
    last_override: u64,
    #[serde(default)]
    schedules: HashMap<String, Schedule>,
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
            return Cache {
                path: Some(path),
                ..Cache::default()
            };
        };
        let saved: Persisted = serde_json::from_str(&text).unwrap_or_default();
        Cache {
            path: Some(path),
            entries: Mutex::new(saved.entries),
            // A file written by a version with one app-wide cooldown simply has
            // no `cooldowns`, so the upgrade starts with a clean slate — which
            // is the right answer for a deadline nobody can attribute any more.
            cooldowns: Mutex::new(saved.cooldowns),
            last_override: Mutex::new(saved.last_override),
            schedules: Mutex::new(saved.schedules),
        }
    }

    /// Best effort: a snapshot that fails to save costs a refetch, nothing more.
    fn save(&self) {
        let (Some(path), Ok(entries), Ok(mut cooldowns), Ok(last_override), Ok(schedules)) = (
            self.path.clone(),
            self.entries.lock(),
            self.cooldowns.lock(),
            self.last_override.lock(),
            self.schedules.lock(),
        ) else {
            return;
        };
        // A cooldown that has run out says nothing, and one belonging to an
        // account that no longer exists says less: neither is worth keeping.
        let now = store::now_ms();
        cooldowns.retain(|_, until| *until > now);
        let saved = Persisted {
            entries: entries.clone(),
            cooldowns: cooldowns.clone(),
            last_override: *last_override,
            schedules: schedules.clone(),
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

    fn in_cooldown(&self, id: &str) -> bool {
        self.retry_at(id).is_some()
    }

    /// When this account may be asked again, if it is in a cooldown.
    pub fn retry_at(&self, id: &str) -> Option<u64> {
        let map = self.cooldowns.lock().ok()?;
        map.get(id).copied().filter(|until| *until > store::now_ms())
    }

    /// The first moment any account comes back, for a UI that has to say
    /// something about the app as a whole. Expired entries are skipped, so a
    /// cooldown nobody cleared cannot go on being reported.
    pub fn soonest_retry(&self) -> Option<u64> {
        let map = self.cooldowns.lock().ok()?;
        let now = store::now_ms();
        map.values().copied().filter(|until| *until > now).min()
    }

    fn start_cooldown(&self, id: &str, for_duration: Duration) {
        if let Ok(mut map) = self.cooldowns.lock() {
            map.insert(id.to_string(), store::now_ms() + for_duration.as_millis() as u64);
        }
        self.save();
    }

    /// Drop one account's cooldown. Its token has just been renewed, so the
    /// refusal that started it was answered with credentials that no longer
    /// exist and says nothing about the request that follows.
    pub fn clear_cooldown(&self, id: &str) {
        if let Ok(mut map) = self.cooldowns.lock() {
            map.remove(id);
        }
        self.save();
    }

    /// Drop every cooldown on the user's own initiative, and say whether there
    /// was anything to drop.
    ///
    /// Someone looking at a card of four-hour-old numbers knows something a
    /// blanket `Retry-After` does not: whether the reason for the refusal still
    /// applies. Tonight it did not — the endpoint had been answering happily
    /// for twenty minutes. Throttled all the same, so this stays a considered
    /// gesture rather than a way to hammer the endpoint.
    pub fn override_cooldowns(&self) -> bool {
        let now = store::now_ms();
        let Ok(mut last) = self.last_override.lock() else {
            return false;
        };
        if now.saturating_sub(*last) < OVERRIDE_INTERVAL_MS {
            return false;
        }
        let Ok(mut map) = self.cooldowns.lock() else {
            return false;
        };
        if !map.values().any(|until| *until > now) {
            // Nothing was blocked, so nothing was spent: the next press still
            // gets its turn.
            return false;
        }
        map.clear();
        *last = now;
        drop(map);
        drop(last);
        self.save();
        true
    }

    pub fn forget(&self, id: &str) {
        if let Ok(mut map) = self.entries.lock() {
            map.remove(id);
        }
        if let Ok(mut map) = self.cooldowns.lock() {
            map.remove(id);
        }
        self.save();
    }

    fn schedule(&self, id: &str) -> Option<Schedule> {
        self.schedules.lock().ok()?.get(id).cloned()
    }

    fn set_schedule(&self, id: &str, plan: Schedule) {
        if let Ok(mut schedules) = self.schedules.lock() {
            schedules.insert(id.to_string(), plan);
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
    if cache.in_cooldown(id) {
        return cache
            .get_any(id)
            .ok_or_else(|| anyhow!(i18n::t("errors.rate_limited")));
    }

    let active = store::active()?;
    let mut credentials = credentials_for(id, active.as_deref())?;

    // An expired token buys nothing but a 401, and this app can renew it
    // itself — so it does, rather than leaving the meter empty until someone
    // next runs `claude` under that account.
    if oauth::due(&credentials, 0) {
        match oauth::renew(id, active.as_deref(), 0).await {
            Ok(Some(renewed)) => credentials = renewed,
            Ok(None) => {}
            // Usage is a nicety: a renewal that failed is worth a line in the
            // log and an attempt with what we have, not an empty card.
            Err(e) => eprintln!("token renewal for {id}: {e:#}"),
        }
    }

    let token =
        access_token_of(&credentials).ok_or_else(|| anyhow!(i18n::t("errors.no_token")))?;

    match fetch(&token).await {
        Ok(usage) => {
            cache.put(id, &usage);
            Ok(usage)
        }
        Err(e) if e.downcast_ref::<Unauthorized>().is_some() => {
            // `expiresAt` said the token was good and the endpoint disagreed;
            // the endpoint is the one that knows. One renewal, one retry.
            let Ok(Some(renewed)) = oauth::renew(id, active.as_deref(), oauth::FORCE).await
            else {
                return Err(e);
            };
            let token =
                access_token_of(&renewed).ok_or_else(|| anyhow!(i18n::t("errors.no_token")))?;
            match fetch(&token).await {
                Ok(usage) => {
                    cache.put(id, &usage);
                    Ok(usage)
                }
                Err(e) => fall_back(cache, id, e),
            }
        }
        Err(e) => fall_back(cache, id, e),
    }
}

/// What a failed fetch leaves the caller with: a rate limit starts the cooldown
/// and hands back whatever was last known, anything else is just the error.
fn fall_back(cache: &Cache, id: &str, e: anyhow::Error) -> Result<Usage> {
    if let Some(limited) = e.downcast_ref::<RateLimited>() {
        cache.start_cooldown(id, limited.cooldown());
        if let Some(stale) = cache.get_any(id) {
            return Ok(stale);
        }
    }
    Err(e)
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

    /// The one date this app reads, in every shape the endpoint has been seen
    /// to write it in. Fractions longer than milliseconds, `Z` or a signed
    /// offset, and the offset in either of its two spellings.
    #[test]
    fn a_reset_instant_parses_however_it_is_written() {
        assert_eq!(epoch_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_ms("1970-01-02T00:00:00Z"), Some(86_400_000));
        assert_eq!(
            epoch_ms("2026-08-30T22:59:59.832072+00:00"),
            Some(1_788_130_799_832)
        );
        // The same instant, said three ways.
        assert_eq!(
            epoch_ms("2026-08-31T00:59:59.832+02:00"),
            Some(1_788_130_799_832)
        );
        assert_eq!(
            epoch_ms("2026-08-30T17:59:59.832-0500"),
            Some(1_788_130_799_832)
        );
        // A leap day, and a date on the far side of the epoch.
        assert_eq!(epoch_ms("2024-02-29T00:00:00Z"), Some(1_709_164_800_000));
        assert_eq!(epoch_ms("1969-12-31T23:59:59Z"), Some(-1_000));
        // Nothing usable is `None`, never a wrong number.
        assert_eq!(epoch_ms("2026-08-30"), None);
        assert_eq!(epoch_ms("not a date"), None);
    }

    /// The launch-time ranking: what is left, over the days it has to last.
    #[test]
    fn weekly_room_is_spread_over_the_days_that_are_left() {
        const NOW: u64 = 1_787_529_600_000; // 2026-08-24T00:00:00Z
        let week = |utilization: f64, resets_at: Option<&str>| Usage {
            five_hour: None,
            seven_day: Some(Window {
                utilization,
                resets_at: resets_at.map(str::to_string),
            }),
            fetched_at: NOW,
        };

        // 20 points to make six days last is a tighter week than 70 for one.
        let tight = week(80.0, Some("2026-08-30T00:00:00Z"))
            .weekly_room_per_day(NOW)
            .expect("ranks");
        let roomy = week(30.0, Some("2026-08-25T00:00:00Z"))
            .weekly_room_per_day(NOW)
            .expect("ranks");
        assert!((tight - 20.0 / 6.0).abs() < 0.01);
        assert_eq!(roomy, 70.0);
        assert!(roomy > tight);

        // Inside the day the divisor stops falling: a window resetting in two
        // hours must not score as if it had eight times the room.
        assert_eq!(
            week(30.0, Some("2026-08-24T02:00:00Z")).weekly_room_per_day(NOW),
            Some(70.0)
        );
        // A reset already past is the same case.
        assert_eq!(
            week(30.0, Some("2026-08-23T00:00:00Z")).weekly_room_per_day(NOW),
            Some(70.0)
        );
        // No date to measure against: assume the whole window is ahead.
        assert_eq!(week(30.0, None).weekly_room_per_day(NOW), Some(10.0));

        // A spent week has no room, and an account with no weekly window
        // cannot be ranked at all.
        assert_eq!(
            week(100.0, Some("2026-08-30T00:00:00Z")).weekly_room_per_day(NOW),
            Some(0.0)
        );
        assert!(parse(r#"{"five_hour": {"utilization": 3.0}}"#)
            .weekly_room_per_day(NOW)
            .is_none());
    }

    /// The cache outlives a cooldown by design, and the auto-switch has to tell
    /// "spent" from "was spent, hours ago".
    #[test]
    fn an_old_reading_is_shown_but_not_acted_on() {
        let mut usage = parse(SAMPLE);
        usage.fetched_at = store::now_ms();
        assert!(usage.is_actionable());

        usage.fetched_at = store::now_ms() - DECISION_MAX_AGE_MS - 1;
        assert!(!usage.is_actionable());
        // Still perfectly readable — the card keeps showing it.
        assert_eq!(usage.five_hour.as_ref().unwrap().utilization, 3.0);
    }

    /// The bar the cards and the tray menu share. It sits well below the one
    /// the auto-switch uses, which is the point: a number worth captioning as
    /// old is still a number worth deciding on.
    #[test]
    fn a_reading_admits_its_age_only_past_the_stale_mark() {
        let mut usage = parse(SAMPLE);
        usage.fetched_at = store::now_ms();
        assert!(!usage.is_stale());

        usage.fetched_at = store::now_ms() - STALE_AFTER_MS - 1;
        assert!(usage.is_stale());
        assert!(usage.is_actionable(), "and still good enough to act on");
    }

    /// The endpoint's blanket hour is not a deadline it keeps to, and sitting
    /// blind for it costs more than the one request finding out costs.
    #[test]
    fn a_cooldown_is_capped_however_long_the_server_asks() {
        let asked_for_an_hour = RateLimited {
            retry_after: Some(Duration::from_secs(3600)),
        };
        assert_eq!(
            asked_for_an_hour.cooldown(),
            Duration::from_millis(MAX_COOLDOWN_MS)
        );

        // Anything shorter is taken at its word.
        let asked_for_a_minute = RateLimited {
            retry_after: Some(Duration::from_secs(60)),
        };
        assert_eq!(asked_for_a_minute.cooldown(), Duration::from_secs(60));
    }

    /// The refusal belongs to the account whose token earned it — pausing the
    /// others blanks the very meters the user is about to switch on.
    /// Every cache here is `default()`, which has no path and therefore never
    /// touches the file the app keeps for the user.
    #[test]
    fn a_cooldown_pauses_one_account_only() {
        let cache = Cache::default();
        cache.start_cooldown("spent", Duration::from_secs(600));

        assert!(cache.in_cooldown("spent"));
        assert!(!cache.in_cooldown("healthy"));
        assert!(cache.retry_at("healthy").is_none());
        assert_eq!(cache.retry_at("spent"), cache.soonest_retry());
    }

    #[test]
    fn a_memory_only_cache_writes_nothing() {
        let cache = Cache::default();
        assert!(cache.path.is_none());
        cache.start_cooldown("spent", Duration::from_secs(600));
        cache.save(); // Would be a write with a path; here it is a no-op.
        assert!(cache.in_cooldown("spent"), "and the state is still in memory");
    }

    #[test]
    fn an_override_lifts_every_cooldown_but_only_now_and_then() {
        let cache = Cache::default();
        cache.start_cooldown("one", Duration::from_secs(600));
        cache.start_cooldown("two", Duration::from_secs(600));

        assert!(cache.override_cooldowns(), "the first press gets through");
        assert!(!cache.in_cooldown("one") && !cache.in_cooldown("two"));

        cache.start_cooldown("one", Duration::from_secs(600));
        assert!(!cache.override_cooldowns(), "the next one is too soon");
        assert!(cache.in_cooldown("one"));
    }

    /// With nothing blocked there is nothing to spend the throttle on.
    #[test]
    fn an_override_with_no_cooldown_running_keeps_its_turn() {
        let cache = Cache::default();
        assert!(!cache.override_cooldowns());

        cache.start_cooldown("one", Duration::from_secs(600));
        assert!(cache.override_cooldowns(), "still its first real use");
    }

    #[test]
    fn a_window_with_no_data_never_triggers_a_switch() {
        let usage = parse(r#"{"five_hour": null, "seven_day": null}"#);
        assert!(usage.hits(1.0, 1.0).is_none());
    }
}

/// One round of the polling task: ask about the accounts that are due, and
/// work out when each should next be asked about.
///
/// The cadence is per account rather than per round because the budget is per
/// account: see `pace` for the measurements the intervals come from. Everything
/// not due reports what is held, and how old that is travels with it.
pub async fn poll_due(cache: &Cache) -> Result<Vec<ProfileUsage>> {
    let profiles = store::list()?;
    let active = store::active().unwrap_or_default();
    let now = store::now_ms();
    let mut out = Vec::with_capacity(profiles.len());

    for profile in profiles {
        let id = profile.id.clone();
        let previous = cache.schedule(&id);
        let due = previous.as_ref().is_none_or(|plan| now >= plan.next_at);

        // Not forced: the TTL sits half a minute under the floor, so anything
        // genuinely due is past it, while a reading the window pulled seconds
        // ago is reused instead of being bought twice. At startup that is the
        // difference between one request per account and two.
        let result = if due {
            for_profile(cache, &id, false).await
        } else {
            match cache.get_any(&id) {
                Some(held) => Ok(held),
                // Never read at all: a meter that has never had a number in it
                // is worse than one request off the budget.
                None => for_profile(cache, &id, true).await,
            }
        };

        if due {
            let binding = result
                .as_ref()
                .ok()
                .and_then(|u| u.binding(profile.five_hour_threshold, profile.seven_day_threshold));
            // A refusal standing right now is one that happened just now; an
            // older one is remembered until it ages out of the trailing hour.
            let last_refusal = if cache.retry_at(&id).is_some() {
                Some(now)
            } else {
                previous.as_ref().and_then(|plan| plan.last_refusal)
            };

            let interval = pace::plan(
                &pace::Sample {
                    binding: binding.map(|(pct, _)| pct),
                    previous: previous.as_ref().and_then(|plan| plan.binding),
                    // With no reading there is no threshold to be near, and the
                    // plan falls through to the movement rules either way.
                    threshold: binding.map_or(100.0, |(_, threshold)| threshold),
                    active: active.as_deref() == Some(id.as_str()),
                    last_interval: previous
                        .as_ref()
                        .map(|plan| Duration::from_secs(plan.interval_s)),
                    last_refusal,
                },
                now,
            );

            cache.set_schedule(
                &id,
                Schedule {
                    next_at: now + interval.as_millis() as u64,
                    interval_s: interval.as_secs(),
                    binding: binding.map(|(pct, _)| pct),
                    last_refusal,
                },
            );
        }

        out.push(entry_for(id, result, cache));
    }

    Ok(out)
}

/// Which accounts a manual round asks the endpoint about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refresh {
    /// Serve what is held and ask only for what has aged past the TTL. What a
    /// window wants when it opens: usually no requests at all.
    Cached,
    /// Every account, now. The user pressed Refresh, which is also the one
    /// gesture allowed to push through a cooldown.
    All,
}

/// Usage for every saved account, one entry each, errors included.
pub async fn for_all(cache: &Cache, refresh: Refresh) -> Result<Vec<ProfileUsage>> {
    let ids: Vec<String> = store::list()?.into_iter().map(|p| p.id).collect();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let result = for_profile(cache, &id, refresh == Refresh::All).await;
        out.push(entry_for(id, result, cache));
    }
    Ok(out)
}

/// One account's reading as the window and the tray need it, whatever it took
/// to get — or not get — it.
fn entry_for(id: String, result: Result<Usage>, cache: &Cache) -> ProfileUsage {
    // Read after the fetch and before the id moves: a refusal in this very
    // round is what sets it.
    let retry_at = cache.retry_at(&id);
    match result {
        Ok(usage) => ProfileUsage {
            id,
            // Old enough to say so: the cooldown, or an account whose numbers
            // have not been refreshed for several rounds running.
            stale: usage.is_stale(),
            usage: Some(usage),
            error: None,
            retry_at,
        },
        Err(e) => ProfileUsage {
            id,
            usage: None,
            error: Some(format!("{e:#}")),
            retry_at,
            stale: false,
        },
    }
}
