//! How often each account's usage may be asked for. Every number in one place.
//!
//! The endpoint does not publish a budget, so these constants are not ours: they
//! come from the measurements documented in `poll_policy.py` of
//! <https://github.com/realiti4/claude-swap>, which probed it deliberately and
//! wrote down the method and the dates. What that work found, in short:
//!
//! - roughly **28-30 requests per hour per identity**, over a **trailing
//!   60-minute window** rather than a bucket that refills. A burst saturates the
//!   identity for up to a full hour, and pausing does not give the headroom
//!   back early — capacity returns only as old requests age out;
//! - the identity is the account (or, in another refusal regime, the token).
//!   Planning for the account is the conservative reading, and it is the one
//!   taken here: a fresh login cannot be relied on to clear a block, and two
//!   machines watching one account share one budget.
//!
//! So the target is an average of about **one request every three minutes per
//! account** — twenty an hour against a cap near thirty — leaving room for the
//! Refresh button, for catching up after a suspend, and for the bounded urgent
//! mode below.
//!
//! This app polled every account once a minute, which is sixty an hour: twice
//! the cap, and it earned exactly the refusals the number predicts.
//!
//! Those measurements can age — the endpoint is undocumented and Anthropic can
//! retune it any day. What would mean this needs revisiting is refusals showing
//! up at these rates.

use std::time::Duration;

use crate::store;

/// Nothing is asked for again inside this, whoever asks. It is also the cache's
/// serve-fresh window, so a window opening, a tray rebuild and a switch in the
/// same minute cost one request between them rather than three.
pub const MIN_INTERVAL: Duration = Duration::from_secs(180);

/// The active account, close to its threshold and visibly burning toward it.
///
/// Bounded by construction rather than by a timer: either the threshold is
/// crossed and the auto-switch moves away, or the movement stops and the next
/// plan decays back to `MIN_INTERVAL`. An episode is worth a handful of extra
/// requests, which the hour's headroom absorbs.
pub const URGENT_INTERVAL: Duration = Duration::from_secs(60);

/// Ceilings for an account whose numbers are not moving: the one in use stays
/// reasonably fresh, an idle alternate is allowed to drift.
pub const ACTIVE_MAX_INTERVAL: Duration = Duration::from_secs(300);
pub const CANDIDATE_MAX_INTERVAL: Duration = Duration::from_secs(600);

/// A spent account is stable enough to ask about slowly, but not to stop asking
/// about: a quota grant or a correction can make it usable before the reset it
/// reported, and the auto-switch must not be left holding a reading old enough
/// to be worthless when that happens.
pub const EXHAUSTED_INTERVAL: Duration = Duration::from_secs(600);

/// Movement, in percentage points between two readings. Below this the account
/// is idle as far as the cadence is concerned — consumption elsewhere, on
/// another machine or another session, shows up here just the same.
const MOVEMENT_DELTA: f64 = 1.0;

/// How close to its threshold the active account has to be for urgent mode.
const ESCALATION_MARGIN: f64 = 15.0;

/// While a refusal is within living memory, no plan goes below this, so freed
/// capacity accumulates instead of being spent the moment it appears.
pub const POST_REFUSAL_MIN_INTERVAL: Duration = Duration::from_secs(360);

/// How long a refusal is remembered. It matches the saturation horizon: a
/// trailing hour takes an hour to age out.
pub const REFUSAL_MEMORY: Duration = Duration::from_secs(3600);

/// While refusals keep coming, each successful poll widens the interval, and a
/// clean run decays it back. The budget is shared with every other machine
/// watching the same account, none of them can see the others, and the endpoint
/// reports no remaining count — so the only way to divide it fairly is for
/// everyone to back off on refusal and creep back on success. It is the same
/// bargain TCP makes, for the same reason.
const BACKOFF_MULT: f64 = 1.5;
pub const BACKOFF_MAX_INTERVAL: Duration = Duration::from_secs(1800);

/// How fast an idle account's interval drifts out toward its ceiling.
const DECAY_MULT: f64 = 1.5;

/// Fraction of jitter on each planned interval, so that two processes watching
/// the same account drift apart instead of arriving together.
const JITTER: f64 = 0.1;

/// Everything the cadence is decided from.
pub struct Sample {
    /// Where the account stands on whichever window is closest to its own
    /// threshold, or `None` when this round could not read it.
    pub binding: Option<f64>,
    /// The same figure at the previous poll, for spotting movement.
    pub previous: Option<f64>,
    pub threshold: f64,
    pub active: bool,
    /// The interval the last plan settled on.
    pub last_interval: Option<Duration>,
    /// When this account was last refused, if it was.
    pub last_refusal: Option<u64>,
}

/// How long to wait before asking about this account again.
pub fn plan(sample: &Sample, now: u64) -> Duration {
    let moved = match (sample.binding, sample.previous) {
        (Some(now_pct), Some(then)) => (now_pct - then).abs() >= MOVEMENT_DELTA,
        // A first reading, or one that failed: neither is evidence of calm.
        _ => true,
    };
    let spent = sample.binding.is_some_and(|pct| pct >= sample.threshold);
    let refused_recently = sample
        .last_refusal
        .is_some_and(|at| now.saturating_sub(at) < REFUSAL_MEMORY.as_millis() as u64);

    // The one case allowed under the floor, which is why it is pinned down so
    // tightly: the account in use, close enough to matter, and visibly moving.
    let urgent = sample.active
        && moved
        && !spent
        && sample
            .binding
            .is_some_and(|pct| pct >= sample.threshold - ESCALATION_MARGIN);

    let mut interval = if spent {
        EXHAUSTED_INTERVAL
    } else if urgent {
        URGENT_INTERVAL
    } else if moved {
        MIN_INTERVAL
    } else {
        // Nothing is happening: drift out toward this account's ceiling.
        let ceiling = if sample.active {
            ACTIVE_MAX_INTERVAL
        } else {
            CANDIDATE_MAX_INTERVAL
        };
        let grown = scale(sample.last_interval.unwrap_or(MIN_INTERVAL), DECAY_MULT);
        grown.min(ceiling).max(MIN_INTERVAL)
    };

    if refused_recently {
        // Back off multiplicatively while the refusals keep coming, and never
        // below the post-refusal floor whatever the rest of the plan said.
        let backed_off = scale(sample.last_interval.unwrap_or(MIN_INTERVAL), BACKOFF_MULT);
        interval = interval
            .max(POST_REFUSAL_MIN_INTERVAL)
            .max(backed_off.min(BACKOFF_MAX_INTERVAL));
    }

    // The floor holds for everything the urgent case is not — and a refusal
    // outranks urgency, since capacity that is not there cannot be spent on
    // being quick.
    if !urgent || interval > URGENT_INTERVAL {
        interval = interval.max(MIN_INTERVAL);
    }

    // The ceilings above are what bounds how old a reading may get, including
    // across a window rollover. Clamping to a reported reset as well would be
    // better, and is what the measured policy does — but the timestamps arrive
    // as ISO-8601 strings and this crate has no date parser, so it is left out
    // rather than approximated.
    jittered(interval.min(BACKOFF_MAX_INTERVAL))
}

fn scale(base: Duration, factor: f64) -> Duration {
    Duration::from_secs_f64(base.as_secs_f64() * factor)
}

/// Deterministic enough to reason about, spread enough to break lockstep: the
/// clock is the only entropy this needs, and pulling in a random number
/// generator for a ±10% nudge would be a dependency for nothing.
fn jittered(interval: Duration) -> Duration {
    let spread = (store::now_ms() % 1000) as f64 / 1000.0 * 2.0 - 1.0;
    scale(interval, 1.0 + spread * JITTER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Sample {
        Sample {
            binding: Some(50.0),
            previous: Some(50.0),
            threshold: 95.0,
            active: true,
            last_interval: Some(MIN_INTERVAL),
            last_refusal: None,
        }
    }

    /// Jitter is ±10%, so every assertion here is about the band, not the
    /// number. This is the check that the band is the one intended.
    fn within(actual: Duration, expected: Duration) -> bool {
        let lo = expected.as_secs_f64() * 0.89;
        let hi = expected.as_secs_f64() * 1.11;
        (lo..=hi).contains(&actual.as_secs_f64())
    }

    /// The floor is the whole point: twenty requests an hour against a cap
    /// near thirty. Nothing below three minutes, whatever else is true.
    #[test]
    fn nothing_is_ever_polled_faster_than_the_floor_except_urgent() {
        let mut s = sample();
        s.binding = Some(10.0);
        s.previous = Some(1.0);
        assert!(within(plan(&s, store::now_ms()), MIN_INTERVAL));

        // Not even the account in use, mid-burn, once it is away from its
        // threshold: urgent mode is for the last stretch, not for every climb.
        s.binding = Some(40.0);
        s.previous = Some(30.0);
        assert!(within(plan(&s, store::now_ms()), MIN_INTERVAL));
    }

    /// The one case the minute was wanted for: the account in use, close to
    /// its threshold, and actually burning toward it.
    #[test]
    fn the_active_account_near_its_threshold_gets_the_minute() {
        let mut s = sample();
        s.binding = Some(90.0);
        s.previous = Some(85.0);
        assert!(within(plan(&s, store::now_ms()), URGENT_INTERVAL));

        // Same distance from the threshold, but nothing is being consumed:
        // there is nothing to be quick about.
        s.previous = Some(90.0);
        assert!(plan(&s, store::now_ms()) > URGENT_INTERVAL);

        // And an account nobody is working under never gets it, however close.
        s.previous = Some(85.0);
        s.active = false;
        assert!(within(plan(&s, store::now_ms()), MIN_INTERVAL));
    }

    /// An idle account drifts out to its ceiling rather than sitting at the
    /// floor forever — and the alternates drift further than the one in use.
    #[test]
    fn an_idle_account_drifts_out_to_its_ceiling() {
        let mut s = sample();
        s.last_interval = Some(ACTIVE_MAX_INTERVAL);
        assert!(within(plan(&s, store::now_ms()), ACTIVE_MAX_INTERVAL));

        s.active = false;
        s.last_interval = Some(CANDIDATE_MAX_INTERVAL);
        assert!(within(plan(&s, store::now_ms()), CANDIDATE_MAX_INTERVAL));
    }

    /// A refusal is remembered for the hour it takes to age out, and while it
    /// is remembered nothing goes back to the floor — not even the urgent case,
    /// which is exactly the one that would re-spend the freed capacity.
    #[test]
    fn a_refusal_holds_the_interval_up_for_the_hour_it_takes_to_clear() {
        let now = store::now_ms();
        let mut s = sample();
        s.binding = Some(90.0);
        s.previous = Some(85.0);
        s.last_refusal = Some(now - 60_000);
        assert!(plan(&s, now) >= scale(POST_REFUSAL_MIN_INTERVAL, 0.89));

        // An hour on, the refusal no longer weighs on the plan.
        s.last_refusal = Some(now - REFUSAL_MEMORY.as_millis() as u64 - 1);
        assert!(within(plan(&s, now), URGENT_INTERVAL));
    }

    /// Repeated refusals widen the interval instead of holding it flat: the
    /// budget is shared with machines this one cannot see.
    #[test]
    fn repeated_refusals_widen_the_interval_toward_the_cap() {
        let now = store::now_ms();
        let mut s = sample();
        s.last_refusal = Some(now - 1_000);
        s.last_interval = Some(Duration::from_secs(600));
        let widened = plan(&s, now);
        assert!(widened > Duration::from_secs(600), "{widened:?}");

        s.last_interval = Some(BACKOFF_MAX_INTERVAL);
        assert!(plan(&s, now) <= scale(BACKOFF_MAX_INTERVAL, 1.11));
    }

    /// A spent account is polled slowly but never abandoned: a grant or a
    /// correction can free it before the reset it reported.
    #[test]
    fn a_spent_account_is_still_asked_about_now_and_then() {
        let mut s = sample();
        s.binding = Some(100.0);
        s.previous = Some(100.0);
        assert!(within(plan(&s, store::now_ms()), EXHAUSTED_INTERVAL));
    }
}
