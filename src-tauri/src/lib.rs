mod actions;
mod claude;
mod i18n;
mod notice;
mod oauth;
mod pace;
mod store;
mod tray;
mod update;
mod usage;

use actions::{AutoSwitch, CurrentAccount, Standstill};
use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;
use store::Profile;
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::ManagerExt;
use usage::{Cache, ProfileUsage};

/// How often the polling task wakes up — not how often anything is fetched.
///
/// Each account carries its own due time, worked out by `pace` from what the
/// endpoint's budget was measured to be; this is only the granularity that
/// scheduler runs at. A minute is fine for it because a minute is the tightest
/// interval `pace` will ever hand out, and a wake-up that finds nothing due
/// costs a lock and a comparison.
const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// How often the token keeper looks at the expiry dates.
///
/// Nothing leaves the machine unless a token is actually due, so this can be
/// far tighter than the usage poll: it is what makes a renewal that a session
/// held the login lock against land seconds after that session lets go, rather
/// than at the next usage tick.
const TOKEN_CHECK_INTERVAL: Duration = Duration::from_secs(20);

/// How often the app looks for a release newer than itself.
///
/// Rare on purpose. A version check is not news that goes stale in minutes, the
/// endpoint is somebody else's and answers unauthenticated requests sixty times
/// an hour per address, and the first pass runs at launch — which for an app
/// that lives in the tray across reboots is the one that matters.
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(6 * 3600);

/// How long after launch the first check runs. Waiting a whole `POLL_INTERVAL`
/// meant an app opened on an account that is already spent sat there doing
/// nothing — and every restart put the clock back to zero. This is just long
/// enough for the window's own first fetch to fill the cache, which is why the
/// first pass reads that cache instead of forcing past it.
const STARTUP_DELAY: Duration = Duration::from_secs(15);

/// Tell the UI and the tray that the store changed.
///
/// Commands and the polling task run off the main thread, and rebuilding the
/// tray menu touches GTK, which on Linux must happen on the main thread.
pub fn notify_changed(app: &AppHandle) {
    refresh_tray(app);
    let _ = app.emit("profiles-changed", ());
}

fn refresh_tray(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || tray::refresh(&handle));
}

/// Refuse the window manager's fullscreen, on the only layer that can see it.
///
/// `maximizable: false` in the config is a no-op under GTK — tao implements it
/// as an empty function — and `Window::is_fullscreen` only reports what the app
/// itself asked for, never what a keyboard shortcut or a tiling extension did.
/// The GTK window's own state signal is the one place the truth shows up.
#[cfg(target_os = "linux")]
fn block_fullscreen(window: &tauri::WebviewWindow) {
    use gtk::gdk::WindowState;
    use gtk::glib;
    use gtk::prelude::*;

    let Ok(gtk_window) = window.gtk_window() else {
        eprintln!("cannot reach the GTK window: fullscreen stays possible");
        return;
    };
    gtk_window.connect_window_state_event(|window, event| {
        if event.new_window_state().contains(WindowState::FULLSCREEN) {
            // Undoing it from inside the signal handler races GTK's own
            // bookkeeping; one turn of the main loop later it sticks.
            let window = window.clone();
            glib::idle_add_local_once(move || window.unfullscreen());
        }
        glib::Propagation::Proceed
    });
}

/// Escape hatch for both rendering workarounds below, for a session where they
/// turn out to be the wrong trade.
#[cfg(target_os = "linux")]
const KEEP_WAYLAND: &str = "CLAUDE_SWITCH_KEEP_WAYLAND";

/// WebKitGTK's DMA-BUF renderer and the NVIDIA proprietary driver disagree
/// under Wayland: the window tears and keeps stale tiles while it is dragged or
/// resized. Turning that path off costs nothing on a UI this small.
///
/// Applied only where the NVIDIA module is actually loaded, and never over a
/// value the user set for themselves.
#[cfg(target_os = "linux")]
fn workaround_nvidia_rendering() {
    const VAR: &str = "WEBKIT_DISABLE_DMABUF_RENDERER";
    if std::env::var_os(VAR).is_none() && std::path::Path::new("/sys/module/nvidia").exists() {
        std::env::set_var(VAR, "1");
    }
}

/// Run through XWayland on a Wayland session that uses fractional scaling.
///
/// GTK 3 has no fractional-scaling support, so under GNOME's
/// `scale-monitor-framebuffer` the compositor and the toolkit disagree about
/// how big the window is. Two things follow, and both were reported here:
/// clicks land beside the system titlebar buttons rather than on them, and
/// dragging the window onto a monitor with a different scale leaves WebKitGTK
/// repainting into a buffer of the old size — the window comes back half drawn.
///
/// XWayland scales the window itself, so the client never sees the change.
/// It is a real trade (no native Wayland input niceties), hence the opt-out.
#[cfg(target_os = "linux")]
fn workaround_fractional_scaling() {
    if std::env::var_os(KEEP_WAYLAND).is_some() {
        return;
    }
    if std::env::var("XDG_SESSION_TYPE").as_deref() != Ok("wayland") {
        return;
    }
    // Reading the setting rather than assuming: a session at a plain integer
    // scale works fine natively and has no reason to pay for this.
    let fractional = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.mutter", "experimental-features"])
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout).contains("scale-monitor-framebuffer")
        })
        .unwrap_or(false);

    if fractional {
        std::env::set_var("GDK_BACKEND", "x11");
    }
}

/// Put the window away without letting the app go with it.
///
/// The hide is queued on the main loop rather than run inside the close
/// handler: the window manager still owns the window at that point, and on
/// Linux a hide issued from there can be swallowed, leaving the window on
/// screen with its close button apparently dead.
fn hide_to_tray(window: &tauri::Window) {
    let handle = window.app_handle().clone();
    let window = window.clone();
    let _ = handle.run_on_main_thread(move || {
        let _ = window.hide();
    });
}

fn to_string_err<T>(r: anyhow::Result<T>) -> Result<T, String> {
    r.map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn list_profiles() -> Result<Vec<Profile>, String> {
    to_string_err(store::list())
}

#[tauri::command]
fn current_account() -> Result<CurrentAccount, String> {
    to_string_err(actions::current_account())
}

#[tauri::command]
fn switch_profile(app: AppHandle, id: String) -> Result<(), String> {
    let result = to_string_err(actions::switch_to(&id));
    notify_changed(&app);
    result
}

#[tauri::command]
fn save_current_account(app: AppHandle, label: Option<String>) -> Result<Profile, String> {
    let result = to_string_err(actions::capture(label));
    notify_changed(&app);
    result
}

#[tauri::command]
fn rename_profile(app: AppHandle, id: String, label: String) -> Result<(), String> {
    let result = to_string_err(actions::rename(&id, &label));
    notify_changed(&app);
    result
}

#[tauri::command]
fn delete_profile(app: AppHandle, id: String) -> Result<(), String> {
    let result = to_string_err(store::delete(&id));
    app.state::<Cache>().forget(&id);
    notify_changed(&app);
    result
}

#[tauri::command]
fn sync_active_profile(app: AppHandle) -> Result<(), String> {
    let result = to_string_err(actions::sync_active());
    notify_changed(&app);
    result
}

#[tauri::command]
fn logout(app: AppHandle) -> Result<(), String> {
    let result = to_string_err(actions::logout_current());
    notify_changed(&app);
    result
}

#[tauri::command]
fn open_login_terminal() -> Result<(), String> {
    to_string_err(actions::open_login_terminal())
}

#[tauri::command]
async fn fetch_usage(
    app: AppHandle,
    cache: State<'_, Cache>,
    force: bool,
) -> Result<Vec<ProfileUsage>, String> {
    let result = usage::for_all(
        &cache,
        if force {
            usage::Refresh::All
        } else {
            usage::Refresh::Cached
        },
    )
    .await;
    // Fresh numbers reach the tray labels and the warning badge too.
    refresh_tray(&app);
    // And they are the same numbers the auto-switch judges on: deciding here as
    // well as on the timer is what makes pressing Refresh on a spent account do
    // what it looks like it should.
    evaluate_auto_switch(&app, &cache).await;
    to_string_err(result)
}

/// What pressing Refresh came to, in full: the tokens, and whether the numbers
/// can be asked for again right now.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RefreshReport {
    tokens: Vec<oauth::Outcome>,
    /// A rate-limit cooldown was lifted for this attempt.
    retried: bool,
    /// When the endpoint may be asked again, if one is still standing.
    retry_at: Option<u64>,
}

/// Renew every token that is due and clear the way for a fresh reading, on the
/// user's own initiative.
///
/// The renewals are the same pass the keeper runs on its timer. What is
/// different is the cooldown: a person pressing this can see the numbers are
/// hours old, which is more than a blanket `Retry-After` knows.
#[tauri::command]
async fn refresh_tokens(app: AppHandle, cache: State<'_, Cache>) -> Result<RefreshReport, String> {
    let tokens = oauth::renew_due(oauth::LIVE_MARGIN_MS, oauth::STORED_MARGIN_MS).await;
    let blocked = cache.soonest_retry().is_some();

    let retried = if tokens.iter().any(|o| o.status == oauth::Status::Renewed) {
        // A renewed account's refusal was answered with a token that no longer
        // exists: there is nothing left for it to say.
        for outcome in &tokens {
            if outcome.status == oauth::Status::Renewed {
                cache.clear_cooldown(&outcome.id);
            }
        }
        blocked
    } else {
        cache.override_cooldowns()
    };

    // The same pass the keeper runs, so the marks on the cards move with it —
    // silently: what this press came to is the window's own to report.
    record_failures(&app, &tokens);

    notify_changed(&app);
    Ok(RefreshReport {
        tokens,
        retried,
        retry_at: cache.soonest_retry(),
    })
}

/// Renew one account now, whatever its expiry says. The explicit gesture from
/// an account's own menu, so it does not second-guess the user.
#[tauri::command]
async fn refresh_profile_token(
    app: AppHandle,
    cache: State<'_, Cache>,
    id: String,
) -> Result<(), String> {
    let active = to_string_err(store::active())?;
    let result = to_string_err(oauth::renew(&id, active.as_deref(), oauth::FORCE).await);
    cache.clear_cooldown(&id);
    if result.is_ok() {
        clear_token_error(&app, &id);
    }
    notify_changed(&app);
    result.map(|_| ())
}

#[tauri::command]
fn set_thresholds(
    app: AppHandle,
    id: String,
    five_hour: f64,
    seven_day: f64,
) -> Result<(), String> {
    let result = to_string_err(store::set_thresholds(&id, five_hour, seven_day));
    notify_changed(&app);
    result
}

#[tauri::command]
fn reorder_profiles(app: AppHandle, ids: Vec<String>) -> Result<(), String> {
    let result = to_string_err(store::reorder(&ids));
    notify_changed(&app);
    result
}

/// Everything the settings dialog shows, read in one round-trip.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    auto_switch: bool,
    start_hidden: bool,
    /// Pick the account with the widest weekly margin at launch.
    start_on_freest: bool,
    autostart: bool,
    /// The saved override, or `null` for "follow the system".
    language: Option<String>,
    /// What "follow the system" currently resolves to, so the dialog can name
    /// it instead of leaving the automatic option unexplained.
    system_language: String,
    /// The language actually in use, override or not — this is what the window
    /// renders in.
    resolved_language: String,
}

/// What the last check found, for a window that opened after it ran.
#[tauri::command]
fn update_status(latest: State<'_, update::Latest>) -> update::Status {
    update::Status {
        current: update::running_version().to_string(),
        available: latest.get(),
    }
}

/// Show what changed, in the browser.
#[tauri::command]
fn open_release_notes(latest: State<'_, update::Latest>) -> Result<(), String> {
    let available = latest.get().ok_or_else(|| i18n::t("errors.no_update"))?;
    to_string_err(update::open_notes(&available))
}

/// Fetch the new package and install it. The password prompt in the middle of
/// this is polkit's own; nothing of it passes through here.
#[tauri::command]
async fn install_update(latest: State<'_, update::Latest>) -> Result<update::Outcome, String> {
    let available = latest
        .get()
        .ok_or_else(|| i18n::t("errors.no_update"))?;
    to_string_err(update::fetch_and_install(&available).await)
}

#[tauri::command]
fn get_settings(app: AppHandle) -> Result<Settings, String> {
    Ok(Settings {
        auto_switch: to_string_err(store::auto_switch())?,
        start_hidden: to_string_err(store::start_hidden())?,
        start_on_freest: to_string_err(store::start_on_freest())?,
        // Absence of the autostart entry is a valid answer, not an error.
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        language: to_string_err(store::language())?,
        system_language: i18n::Lang::from_tag(&i18n::system_tag()).tag().to_string(),
        resolved_language: i18n::lang().tag().to_string(),
    })
}

/// `None` hands the choice back to the system locale.
#[tauri::command]
fn set_language(app: AppHandle, tag: Option<String>) -> Result<(), String> {
    let result = to_string_err(store::set_language(tag.as_deref()));
    i18n::invalidate();
    // The tray menu is built from the same catalog, so it has to be redrawn.
    notify_changed(&app);
    result
}

#[tauri::command]
fn set_auto_switch(app: AppHandle, enabled: bool) -> Result<(), String> {
    let result = to_string_err(store::set_auto_switch(enabled));
    notify_changed(&app);
    result
}

#[tauri::command]
fn set_start_hidden(enabled: bool) -> Result<(), String> {
    to_string_err(store::set_start_hidden(enabled))
}

#[tauri::command]
fn set_start_on_freest(enabled: bool) -> Result<(), String> {
    to_string_err(store::set_start_on_freest(enabled))
}

/// The launch choice, held until the window comes and asks for it.
#[derive(Default)]
struct StartupPick(std::sync::Mutex<Option<actions::Picked>>);

impl StartupPick {
    fn set(&self, picked: actions::Picked) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(picked);
        }
    }

    /// Handed over once. A launch is news exactly one time, and the window
    /// re-reads this every time the language changes.
    fn take(&self) -> Option<actions::Picked> {
        self.0.lock().ok().and_then(|mut slot| slot.take())
    }
}

/// What the launch-time choice came to, for a window that was not up when it
/// was made — which, with "start in the bar", can be hours later.
///
/// The same shape as the update check, for the same reason: the event announcing
/// it can be emitted before the webview has a listener, so the window also asks
/// on its way up.
#[tauri::command]
fn startup_pick(pick: State<'_, StartupPick>) -> Option<actions::Picked> {
    pick.take()
}

/// Whatever was raised while there was nowhere to show it — a window opening
/// hours after the fact is the only reader those messages will get.
#[tauri::command]
fn pending_notices(pending: State<'_, notice::Pending>) -> Vec<notice::Notice> {
    pending.take()
}

/// The failing accounts, for a window that opened after the failure.
#[tauri::command]
fn token_errors(errors: State<'_, TokenErrors>) -> HashMap<String, String> {
    errors.snapshot()
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|e| i18n::t_args("errors.autostart", &[("error", &e.to_string())]))
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct AutoSwitched {
    from: String,
    to: String,
    reason: String,
}

/// The standstill already announced, so "nothing left to switch to" is said
/// once instead of once a minute.
///
/// The evaluation runs on every tick and on every Refresh, and while nobody is
/// working its answer is the same answer: an account over its threshold with no
/// unspent account behind it stays that way for hours. The sentence is worth
/// one notification and no more — the rest of the telling is the window, where
/// the meters stay full and the tray icon keeps its warning.
///
/// Not persisted, for the same reason the token failures are not: a restart is
/// entitled to say it again, and by then the app has stopped watching anyway.
#[derive(Default)]
struct Announced(std::sync::Mutex<Option<Standstill>>);

impl Announced {
    /// Returns whether this is news, and remembers it either way. `None` ends
    /// the episode: whatever comes next is news again.
    fn set(&self, standstill: Option<Standstill>) -> bool {
        match self.0.lock() {
            Ok(mut slot) if *slot != standstill => {
                *slot = standstill;
                true
            }
            _ => false,
        }
    }
}

/// Check the active account against its thresholds, and tell the UI whatever
/// came of it. Cheap and silent when there is nothing to do, so it is safe to
/// call from anywhere fresh numbers arrive.
async fn evaluate_auto_switch(app: &AppHandle, cache: &Cache) {
    let outcome = actions::auto_switch(cache).await;
    let announced = app.state::<Announced>();
    match outcome {
        Ok(AutoSwitch::Switched { from, to, reason }) => {
            // Somewhere to go after all: whatever standstill was announced is
            // over, and the next one is worth hearing about.
            announced.set(None);
            notify_changed(app);
            // Two different things: the event is data, for a window that has a
            // list to redraw, and it is dropped when there is no window to hear
            // it. The sentence is for the user, and goes wherever they are.
            notice::info(
                app,
                i18n::t_args(
                    "autoswitch.switched",
                    &[("to", &to), ("from", &from), ("reason", &reason)],
                ),
            );
            let _ = app.emit("auto-switched", AutoSwitched { from, to, reason });
        }
        // Nothing to redraw here — the message is the whole of it, and only
        // the first time. What ends the silence is the situation changing: a
        // window resetting, an account added or freed, the user moving
        // themselves — all of which come back through one of the arms below.
        Ok(AutoSwitch::Exhausted { reason, standstill }) => {
            if announced.set(Some(standstill)) {
                notice::info(
                    app,
                    i18n::t_args("autoswitch.exhausted", &[("reason", &reason)]),
                );
            }
        }
        // Under threshold again, or not in the business of switching at all:
        // either way there is no standstill to be holding on to.
        Ok(AutoSwitch::Idle) => {
            announced.set(None);
        }
        // This pass learned nothing, which is not the same as learning that
        // the situation changed. Leave the latch as it was.
        Ok(AutoSwitch::Blind) => {}
        // A failed check is usually a network blip; the next tick retries.
        Err(e) => eprintln!("auto-switch: {e:#}"),
    }
}

/// Put the app on the account with the widest weekly margin, once, at launch.
///
/// A task of its own rather than a step in `poll_usage`: that one waits out
/// `STARTUP_DELAY` first, and a switch that lands fifteen seconds into a session
/// lands under the user's hands. This runs immediately and buys its own numbers
/// — the window's first fetch, moments later, is served from the same cache
/// entries rather than paying for them twice.
async fn choose_startup_account(app: AppHandle) {
    let cache = app.state::<Cache>();
    match actions::start_on_freest(&cache).await {
        Ok(Some(picked)) => {
            app.state::<StartupPick>().set(picked.clone());
            notify_changed(&app);
            let _ = app.emit("startup-picked", picked);
        }
        // Off, nothing readable, or already on the freest account: all three
        // are the same silence.
        Ok(None) => {}
        Err(e) => eprintln!("startup account: {e:#}"),
    }
}

/// Keep usage fresh, and rotate away from the active account once it is spent.
///
/// The refresh happens whether or not auto-switch is on: the tray labels and the
/// warning badge are worth keeping current on their own.
async fn poll_usage(app: AppHandle) {
    tokio::time::sleep(STARTUP_DELAY).await;
    loop {
        let cache = app.state::<Cache>();
        match usage::poll_due(&cache).await {
            // Pushed to the window rather than left for it to come and find.
            // Its own timer would be the webview's, and the webview spends most
            // of this app's life hidden in the tray, where timers are throttled
            // or stopped outright — this task is not.
            Ok(entries) => {
                let _ = app.emit("usage-updated", entries);
            }
            Err(e) => eprintln!("usage refresh: {e:#}"),
        }
        refresh_tray(&app);
        evaluate_auto_switch(&app, &cache).await;
        drop(cache);

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look for a newer release, at launch and a few times a day after that.
async fn poll_updates(app: AppHandle) {
    loop {
        match update::check().await {
            Ok(found) => {
                let latest = app.state::<update::Latest>();
                // Only when it changes: the check outlives many windows, and
                // re-announcing the same version every six hours would make the
                // badge something to dismiss rather than something to read.
                if latest.set(found.clone()) {
                    if let Some(available) = found {
                        let _ = app.emit("update-available", available);
                    }
                }
            }
            // Offline, rate-limited, or the API changed shape. None of it is
            // worth interrupting anyone over, and the next pass costs nothing.
            Err(e) => eprintln!("update check: {e:#}"),
        }

        tokio::time::sleep(UPDATE_CHECK_INTERVAL).await;
    }
}

/// Which saved accounts cannot renew their token, and why.
///
/// Not persisted: the keeper looks at every account within seconds of launch, so
/// a failure that has stopped happening has no business outliving the process
/// that saw it. The window reads this on its way up and is pushed every change
/// after that.
#[derive(Default)]
struct TokenErrors(std::sync::Mutex<HashMap<String, String>>);

/// What one renewal pass did to the map.
#[derive(Default)]
struct Folded {
    /// It moved, so the window has to be told.
    changed: bool,
    /// The accounts that were not already failing — the only ones worth a word.
    fresh: Vec<String>,
}

impl TokenErrors {
    /// Fold one renewal pass in.
    ///
    /// A pass reports on every saved account, so an account it does not mention
    /// has been deleted and its mark goes with it. A renewal clears the mark and
    /// a failure sets it; a deferral — the login lock was held, and the next
    /// pass is seconds away — leaves whatever was there, because it learned
    /// nothing either way. Without that the mark would blink off and back on
    /// every time Claude Code happened to be writing its own login.
    fn fold(&self, outcomes: &[oauth::Outcome]) -> Folded {
        let Ok(mut held) = self.0.lock() else {
            return Folded::default();
        };

        let mut next = HashMap::new();
        let mut fresh = Vec::new();
        for outcome in outcomes {
            match outcome.status {
                oauth::Status::Failed => {
                    if !held.contains_key(&outcome.id) {
                        fresh.push(outcome.id.clone());
                    }
                    next.insert(
                        outcome.id.clone(),
                        outcome
                            .error
                            .clone()
                            .unwrap_or_else(|| i18n::t("errors.no_token")),
                    );
                }
                oauth::Status::Deferred => {
                    if let Some(error) = held.get(&outcome.id) {
                        next.insert(outcome.id.clone(), error.clone());
                    }
                }
                _ => {}
            }
        }

        let changed = *held != next;
        if changed {
            *held = next;
        }
        Folded { changed, fresh }
    }

    /// One account renewed by hand, out of step with the keeper's pass.
    fn forget(&self, id: &str) -> bool {
        self.0
            .lock()
            .map(|mut held| held.remove(id).is_some())
            .unwrap_or(false)
    }

    fn snapshot(&self) -> HashMap<String, String> {
        self.0.lock().map(|held| held.clone()).unwrap_or_default()
    }
}

/// Fold one renewal pass into the failure map, push it to the window when it
/// moved, and hand back the accounts whose failure is new.
fn record_failures<'a>(
    app: &AppHandle,
    outcomes: &'a [oauth::Outcome],
) -> Vec<&'a oauth::Outcome> {
    let errors = app.state::<TokenErrors>();
    let folded = errors.fold(outcomes);
    if folded.changed {
        let _ = app.emit("token-errors", errors.snapshot());
    }
    outcomes
        .iter()
        .filter(|o| folded.fresh.contains(&o.id))
        .collect()
}

/// Drop one account's mark and tell the window, after a renewal that worked.
fn clear_token_error(app: &AppHandle, id: &str) {
    let errors = app.state::<TokenErrors>();
    if errors.forget(id) {
        let _ = app.emit("token-errors", errors.snapshot());
    }
}

/// Keep every saved account's token alive.
///
/// Claude Code renews only the login it is running under, and only while it is
/// running: without this, a stored account's token expires a few hours after
/// the last switch away from it, its meters go blank, and the auto-switch is
/// left choosing between accounts it cannot read. Most passes do nothing but
/// read a handful of expiry dates.
async fn keep_tokens_fresh(app: AppHandle) {
    loop {
        let outcomes = oauth::renew_due(oauth::LIVE_MARGIN_MS, oauth::STORED_MARGIN_MS).await;

        // The same failure comes back every pass, twenty seconds apart; only the
        // ones that are new to this one are worth interrupting anyone about.
        // The rest of the telling is the mark on the card, which stays put.
        for outcome in record_failures(&app, &outcomes) {
            eprintln!(
                "token renewal for {}: {}",
                outcome.id,
                outcome.error.as_deref().unwrap_or("failed")
            );
            notice::error(
                &app,
                i18n::t_args(
                    "tokens.failed",
                    &[
                        ("name", &outcome.label),
                        ("error", outcome.error.as_deref().unwrap_or("")),
                    ],
                ),
            );
        }

        // The expiry the cards show comes from the store, and a renewal moved it.
        if outcomes.iter().any(|o| o.status == oauth::Status::Renewed) {
            notify_changed(&app);
        }

        tokio::time::sleep(TOKEN_CHECK_INTERVAL).await;
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "linux")]
    {
        workaround_fractional_scaling();
        workaround_nvidia_rendering();
    }

    tauri::Builder::default()
        // First, before anything else has a chance to run: a second launch has
        // no business building a tray icon or loading a cache it is about to
        // throw away. What it does instead is what clicking a running app's
        // icon should do — bring the window back — and then exit.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            // Onto the main thread, as everything that touches the window does
            // here: this callback arrives on whichever thread the plugin
            // listens on, and GTK has opinions about that.
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || tray::show_window(&handle));
        }))
        .plugin(tauri_plugin_autostart::init(
            // Launch through the login session's own mechanism, with no extra
            // arguments: the start-hidden setting decides what the app does.
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // Rust-side only: nothing in the webview posts a notification, so the
        // window's capabilities stay as they are.
        .plugin(tauri_plugin_notification::init())
        .manage(Cache::load())
        .manage(update::Latest::default())
        .manage(StartupPick::default())
        .manage(notice::Pending::default())
        .manage(Announced::default())
        .manage(TokenErrors::default())
        .invoke_handler(tauri::generate_handler![
            list_profiles,
            current_account,
            switch_profile,
            save_current_account,
            rename_profile,
            delete_profile,
            sync_active_profile,
            logout,
            open_login_terminal,
            fetch_usage,
            refresh_tokens,
            refresh_profile_token,
            set_thresholds,
            reorder_profiles,
            get_settings,
            set_auto_switch,
            set_start_hidden,
            set_start_on_freest,
            startup_pick,
            pending_notices,
            token_errors,
            set_autostart,
            set_language,
            update_status,
            install_update,
            open_release_notes,
        ])
        .setup(|app| {
            tray::build(app.handle())?;
            tray::refresh(app.handle());

            #[cfg(target_os = "linux")]
            if let Some(window) = app.get_webview_window("main") {
                block_fullscreen(&window);
            }

            // The window is created hidden so that starting in the tray never
            // flashes it; show it now unless that is what the user asked for.
            if !store::start_hidden().unwrap_or(false) {
                tray::show_window(app.handle());
            }

            // Before the polling task, and before the user has touched
            // anything: this is the one decision that has to be made while
            // "at launch" is still true.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(choose_startup_account(handle));
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(poll_usage(handle));
            // Straight away, with no startup delay: an account whose token
            // expired while the app was closed is exactly the one whose meters
            // would otherwise stay empty until something else asked.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(keep_tokens_fresh(handle));
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(poll_updates(handle));
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Closing the window leaves the app alive in the tray.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                hide_to_tray(window);
            }
            // A list of a handful of accounts gains nothing from a full
            // screen. On macOS the window's own state is trustworthy, so
            // stepping back out on resize is enough; Linux needs the GTK-level
            // guard installed in `setup` instead.
            #[cfg(not(target_os = "linux"))]
            WindowEvent::Resized(_) => {
                if window.is_fullscreen().unwrap_or(false) {
                    let _ = window.set_fullscreen(false);
                }
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while building the application")
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested { api, code, .. } => {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
            // Clicking the Dock icon of an app with no visible window: on macOS
            // that is the gesture for "bring it back", and without this the
            // click would do nothing once the window is in the menu bar.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => tray::show_window(app),
            _ => {
                let _ = app;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(id: &str, status: oauth::Status, error: Option<&str>) -> oauth::Outcome {
        oauth::Outcome {
            id: id.to_string(),
            label: id.to_string(),
            status,
            error: error.map(str::to_string),
        }
    }

    /// The mark on a card is a state, and the user hears about it once. A
    /// failure that is still failing is not news a second time.
    #[test]
    fn a_failure_is_new_only_once() {
        let errors = TokenErrors::default();
        let pass = [outcome("work", oauth::Status::Failed, Some("refused"))];

        let first = errors.fold(&pass);
        assert!(first.changed, "the first failure has to reach the window");
        assert_eq!(first.fresh, ["work"]);

        let second = errors.fold(&pass);
        assert!(!second.changed, "the same failure moved nothing");
        assert!(second.fresh.is_empty(), "and is not worth saying twice");
        assert_eq!(errors.snapshot()["work"], "refused");
    }

    /// A deferral is Claude Code holding its own login lock for a moment. Left
    /// to clear the mark it would blink off and back on — and announce itself
    /// again on the way back.
    #[test]
    fn a_deferral_leaves_the_mark_alone() {
        let errors = TokenErrors::default();
        errors.fold(&[outcome("work", oauth::Status::Failed, Some("refused"))]);

        let deferred = errors.fold(&[outcome("work", oauth::Status::Deferred, None)]);
        assert!(!deferred.changed);
        assert_eq!(errors.snapshot()["work"], "refused");

        let renewed = errors.fold(&[outcome("work", oauth::Status::Renewed, None)]);
        assert!(renewed.changed, "a renewal is what actually clears it");
        assert!(errors.snapshot().is_empty());
    }

    /// `renew_due` reports on every saved account, so an account missing from a
    /// pass is one that has been deleted.
    #[test]
    fn a_deleted_account_takes_its_mark_with_it() {
        let errors = TokenErrors::default();
        errors.fold(&[
            outcome("work", oauth::Status::Failed, Some("refused")),
            outcome("personal", oauth::Status::Failed, Some("refused")),
        ]);

        let after = errors.fold(&[outcome("work", oauth::Status::Failed, Some("refused"))]);
        assert!(after.changed);
        assert!(after.fresh.is_empty(), "work was already failing");
        assert_eq!(errors.snapshot().keys().collect::<Vec<_>>(), ["work"]);
    }

    fn standstill(account: &str, window: usage::Kind) -> Standstill {
        Standstill {
            account: account.to_string(),
            window,
        }
    }

    /// The evaluation runs every minute and on every Refresh, and while every
    /// account is spent it keeps reaching the same conclusion. The user hears
    /// it once.
    #[test]
    fn a_standstill_is_announced_once() {
        let announced = Announced::default();
        let stuck = standstill("work", usage::Kind::FiveHour);

        assert!(announced.set(Some(stuck.clone())));
        assert!(!announced.set(Some(stuck.clone())), "the same standstill");
        assert!(!announced.set(Some(stuck)), "and still the same one");
    }

    /// What ends the silence: the window resets, an account is added or freed,
    /// the user moves themselves. All of them come back as an evaluation that
    /// is no longer a standstill, and the next one is news again.
    #[test]
    fn a_standstill_that_ends_is_news_when_it_returns() {
        let announced = Announced::default();
        let stuck = standstill("work", usage::Kind::FiveHour);

        announced.set(Some(stuck.clone()));
        announced.set(None);
        assert!(announced.set(Some(stuck)));
    }

    /// Being stuck somewhere else, or stuck on the other window, is a different
    /// sentence about a different situation — not a repeat.
    #[test]
    fn a_different_standstill_is_worth_saying() {
        let announced = Announced::default();

        assert!(announced.set(Some(standstill("work", usage::Kind::FiveHour))));
        assert!(announced.set(Some(standstill("personal", usage::Kind::FiveHour))));
        assert!(announced.set(Some(standstill("personal", usage::Kind::SevenDay))));
    }
}
