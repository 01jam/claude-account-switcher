//! Business logic shared by the Tauri commands and the tray menu.

use anyhow::{anyhow, Result};
use serde::Serialize;

use crate::claude;
use crate::i18n;
use crate::store::{self, Meta, Profile};
use crate::usage;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CurrentAccount {
    pub logged_in: bool,
    pub email: Option<String>,
    pub subscription: Option<String>,
    pub expires_at: Option<u64>,
    /// Id of the saved profile this login belongs to, if it is one of them.
    pub profile_id: Option<String>,
    /// True when Claude Code holds a login the app has never saved.
    pub unsaved: bool,
}

pub fn current_account() -> Result<CurrentAccount> {
    let credentials = claude::read_credentials()?;
    let account = claude::read_account_keys()?;
    let email = claude::email_of(&account);

    let profile_id = store::list()?
        .into_iter()
        .find(|p| p.email.is_some() && p.email == email)
        .map(|p| p.id);

    Ok(CurrentAccount {
        logged_in: credentials.is_some(),
        subscription: credentials.as_ref().and_then(claude::subscription_of),
        expires_at: credentials.as_ref().and_then(claude::expires_at_of),
        unsaved: credentials.is_some() && profile_id.is_none(),
        email,
        profile_id,
    })
}

/// Copy the live credentials back into the active profile. Claude Code rotates
/// the OAuth tokens as it runs, so without this a switch would restore a stale
/// refresh token and force a re-login.
pub fn sync_active() -> Result<()> {
    let Some(id) = store::active()? else {
        return Ok(());
    };
    if !store::exists(&id) {
        return Ok(());
    }
    let Some(credentials) = claude::read_credentials()? else {
        return Ok(());
    };
    let account = claude::read_account_keys()?;
    let mut meta = store::load_meta(&id)?;

    // The live login may belong to a different account if the user logged in
    // outside the app; never let it overwrite the wrong profile.
    let live_email = claude::email_of(&account);
    if meta.email.is_some() && live_email.is_some() && meta.email != live_email {
        return Ok(());
    }

    meta.email = live_email.or(meta.email);
    meta.subscription = claude::subscription_of(&credentials).or(meta.subscription);
    meta.expires_at = claude::expires_at_of(&credentials);
    store::save(&id, &meta, &credentials, &account)
}

pub fn switch_to(id: &str) -> Result<()> {
    if !store::exists(id) {
        return Err(anyhow!("account not found: {id}"));
    }
    if store::active()?.as_deref() == Some(id) {
        return Ok(());
    }

    sync_active()?;
    store::backup(&claude::read_credentials()?, &claude::read_account_keys()?)?;

    let credentials = store::load_credentials(id)?;
    let account = store::load_account(id)?;
    claude::write_credentials(&credentials)?;
    claude::write_account_keys(&account)?;

    let mut meta = store::load_meta(id)?;
    meta.last_used = Some(store::now_ms());
    store::save_meta(id, &meta)?;
    store::set_active(Some(id))
}

/// Save whatever Claude Code is logged into right now as a profile. Re-saving an
/// account already stored refreshes it in place instead of duplicating it.
pub fn capture(label: Option<String>) -> Result<Profile> {
    let Some(credentials) = claude::read_credentials()? else {
        return Err(anyhow!(i18n::t("errors.no_login")));
    };
    let account = claude::read_account_keys()?;
    let email = claude::email_of(&account);
    let subscription = claude::subscription_of(&credentials);
    let expires_at = claude::expires_at_of(&credentials);

    let existing = store::list()?
        .into_iter()
        .find(|p| p.email.is_some() && p.email == email);

    let (id, mut meta) = match existing {
        Some(p) => {
            let meta = store::load_meta(&p.id)?;
            (p.id, meta)
        }
        None => {
            let label = label
                .clone()
                .filter(|l| !l.trim().is_empty())
                .or_else(|| email.clone())
                .unwrap_or_else(|| i18n::t("accounts.default_label"));
            let id = store::unique_id(&label)?;
            (
                id,
                Meta {
                    label,
                    email: None,
                    subscription: None,
                    created_at: store::now_ms(),
                    last_used: None,
                    expires_at: None,
                    five_hour_threshold: store::DEFAULT_THRESHOLD,
                    seven_day_threshold: store::DEFAULT_THRESHOLD,
                    order: None,
                },
            )
        }
    };

    if let Some(l) = label.filter(|l| !l.trim().is_empty()) {
        meta.label = l.trim().to_string();
    }
    meta.email = email;
    meta.subscription = subscription;
    meta.expires_at = expires_at;
    meta.last_used = Some(store::now_ms());

    store::save(&id, &meta, &credentials, &account)?;
    store::set_active(Some(&id))?;

    store::list()?
        .into_iter()
        .find(|p| p.id == id)
        .ok_or_else(|| anyhow!("profile disappeared after saving"))
}

/// The account after `id` in the user's rotation order, wrapping around.
pub fn rotation_after(id: &str) -> Result<Vec<Profile>> {
    let profiles = store::list()?;
    let Some(index) = profiles.iter().position(|p| p.id == id) else {
        return Ok(profiles);
    };
    let mut rotated = profiles;
    rotated.rotate_left(index + 1);
    rotated.retain(|p| p.id != id);
    Ok(rotated)
}

/// Outcome of one auto-switch evaluation, for logging and notifications.
pub enum AutoSwitch {
    /// Nothing to do: disabled, no active account, or still under threshold.
    Idle,
    /// Moved from one account to another because a window filled up.
    Switched {
        from: String,
        to: String,
        reason: String,
    },
    /// Threshold reached but no account left to move to.
    Exhausted { reason: String },
}

/// Check the active account against its thresholds and rotate if it is spent.
///
/// Candidates are tried in the user's order; one whose own usage is already past
/// its thresholds is skipped. An account whose usage cannot be read is taken as
/// usable — better to try a switch than to stall on a network error.
pub async fn auto_switch(cache: &usage::Cache) -> Result<AutoSwitch> {
    if !store::auto_switch()? {
        return Ok(AutoSwitch::Idle);
    }
    let Some(active_id) = store::active()? else {
        return Ok(AutoSwitch::Idle);
    };
    let Ok(active_meta) = store::load_meta(&active_id) else {
        return Ok(AutoSwitch::Idle);
    };

    let current = usage::for_profile(cache, &active_id, false).await?;
    // A rate-limit cooldown keeps the last known numbers on screen for as long
    // as an hour. Fine for a card that says how old they are; no basis at all
    // for moving the user off an account whose window may have reset since.
    if !current.is_actionable() {
        return Ok(AutoSwitch::Idle);
    }
    let Some(hit) = current.hits(
        active_meta.five_hour_threshold,
        active_meta.seven_day_threshold,
    ) else {
        return Ok(AutoSwitch::Idle);
    };

    let reason = i18n::t_args(
        "autoswitch.reason",
        &[
            ("window", &hit.kind.label()),
            ("used", &format!("{:.0}", hit.utilization)),
            ("threshold", &format!("{:.0}", hit.threshold)),
        ],
    );

    for candidate in rotation_after(&active_id)? {
        let spent = match usage::for_profile(cache, &candidate.id, false).await {
            Ok(u) => u
                .hits(candidate.five_hour_threshold, candidate.seven_day_threshold)
                .is_some(),
            Err(_) => false,
        };
        if spent {
            continue;
        }
        switch_to(&candidate.id)?;
        return Ok(AutoSwitch::Switched {
            from: active_meta.label,
            to: candidate.label,
            reason,
        });
    }

    Ok(AutoSwitch::Exhausted { reason })
}

/// The account launch settled on, and the number it was settled by — what the
/// window says about a switch nobody pressed a button for.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Picked {
    pub to: String,
    /// Weekly utilisation at the moment of the choice.
    pub used: f64,
}

/// One account in the running, with the figure it is ranked on.
struct Candidate {
    profile: Profile,
    room: f64,
    used: f64,
    active: bool,
}

/// Start on the account with the widest weekly margin.
///
/// "Widest" is `Usage::weekly_room_per_day`: what the week has left over the
/// days it still has to cover — see there for why those two are one number
/// rather than a percentage and a caveat. Runs once, at launch, and only with
/// the setting on; from then on the rotation is the auto-switch's business.
///
/// An account whose usage cannot be read is not a candidate — this is a
/// comparison, and one made against a blank is not one. That holds for the
/// account already in place too: it cannot defend its turn with numbers nobody
/// can see. With nothing readable at all, or with the freest account already
/// the active one, nothing happens and nothing is said.
pub async fn start_on_freest(cache: &usage::Cache) -> Result<Option<Picked>> {
    if !store::start_on_freest()? {
        return Ok(None);
    }
    // A login the app has never saved is not ours to move: switching would file
    // it away under `backups/` and take the user off the account they signed in
    // to. The window asks them to save it first, and until they do this waits.
    if current_account()?.unsaved {
        return Ok(None);
    }

    let active = store::active()?;
    let now = store::now_ms();
    let mut ranked: Vec<Candidate> = Vec::new();

    for profile in store::list()? {
        // Cached, not forced: at launch the cache is usually cold, so this is
        // one request per account — and the window's own first fetch, moments
        // later, is served from these same entries rather than buying them
        // again.
        let Ok(usage) = usage::for_profile(cache, &profile.id, false).await else {
            continue;
        };
        // The bar the auto-switch holds itself to, for the same reason: a
        // reading old enough to have been overtaken cannot move anyone.
        if !usage.is_actionable() {
            continue;
        }
        let (Some(room), Some(week)) = (usage.weekly_room_per_day(now), usage.seven_day.as_ref())
        else {
            continue;
        };
        ranked.push(Candidate {
            active: active.as_deref() == Some(profile.id.as_str()),
            used: week.utilization,
            room,
            profile,
        });
    }

    // Widest margin first, and on a tie the account already in place keeps it:
    // a switch that buys nothing is still a switch, with a backup and a
    // rewritten login behind it.
    ranked.sort_by(|a, b| {
        b.room
            .partial_cmp(&a.room)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.active.cmp(&a.active))
    });

    match ranked.first() {
        Some(best) if !best.active => {
            switch_to(&best.profile.id)?;
            Ok(Some(Picked {
                to: best.profile.label.clone(),
                used: best.used,
            }))
        }
        _ => Ok(None),
    }
}

/// Clear the live login so `claude` prompts for a fresh one. The active profile
/// keeps its own copy, so this is not destructive.
pub fn logout_current() -> Result<()> {
    sync_active()?;
    store::backup(&claude::read_credentials()?, &claude::read_account_keys()?)?;
    claude::remove_credentials()?;
    claude::write_account_keys(&serde_json::Value::Object(Default::default()))?;
    store::set_active(None)
}

pub fn rename(id: &str, label: &str) -> Result<()> {
    let label = label.trim();
    if label.is_empty() {
        return Err(anyhow!(i18n::t("errors.empty_name")));
    }
    let mut meta = store::load_meta(id)?;
    meta.label = label.to_string();
    store::save_meta(id, &meta)
}

const TERMINALS: &[(&str, &[&str])] = &[
    ("gnome-terminal", &["--"]),
    ("konsole", &["-e"]),
    ("xfce4-terminal", &["-e"]),
    ("tilix", &["-e"]),
    ("kitty", &[]),
    ("alacritty", &["-e"]),
    ("wezterm", &["start", "--"]),
    ("x-terminal-emulator", &["-e"]),
    ("xterm", &["-e"]),
];

/// What the terminal runs: the login, then a note, then a shell left open so
/// anything `claude` printed stays readable.
fn login_command() -> String {
    format!(
        "claude; echo; echo '{}'; exec ${{SHELL:-bash}}",
        i18n::t("login.terminal_hint").replace('\'', "'\\''")
    )
}

/// Open a terminal running `claude` so the user can complete the OAuth login.
#[cfg(not(target_os = "macos"))]
pub fn open_login_terminal() -> Result<()> {
    use std::process::Command;

    let shell_cmd = login_command();

    for (bin, args) in TERMINALS {
        if which(bin).is_none() {
            continue;
        }
        let mut cmd = Command::new(bin);
        cmd.args(*args).arg("bash").arg("-lc").arg(&shell_cmd);
        if cmd.spawn().is_ok() {
            return Ok(());
        }
    }
    Err(anyhow!(i18n::t("errors.no_terminal")))
}

/// macOS has one terminal worth assuming, and AppleScript is the only way to
/// hand it a command; `open -a Terminal` can launch the app but not a command.
#[cfg(target_os = "macos")]
pub fn open_login_terminal() -> Result<()> {
    use std::process::Command;

    // Doubling the backslashes and quotes: the string travels through
    // AppleScript's own literal syntax before it reaches the shell.
    let inner = login_command().replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("tell application \"Terminal\"\nactivate\ndo script \"{inner}\"\nend tell");

    let ok = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        return Ok(());
    }

    // Last resort: an empty terminal is still better than nothing — the user
    // can type `claude` into it.
    if Command::new("open").arg("-a").arg("Terminal").status().is_ok() {
        return Ok(());
    }
    Err(anyhow!(i18n::t("errors.no_terminal")))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn which(bin: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|dir| dir.join(bin))
        .find(|p| p.is_file())
}
