mod actions;
mod claude;
mod i18n;
mod oauth;
mod pace;
mod store;
mod tray;
mod update;
mod usage;

use actions::{AutoSwitch, CurrentAccount};
use serde::Serialize;
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

/// Check the active account against its thresholds, and tell the UI whatever
/// came of it. Cheap and silent when there is nothing to do, so it is safe to
/// call from anywhere fresh numbers arrive.
async fn evaluate_auto_switch(app: &AppHandle, cache: &Cache) {
    match actions::auto_switch(cache).await {
        Ok(AutoSwitch::Switched { from, to, reason }) => {
            notify_changed(app);
            let _ = app.emit("auto-switched", AutoSwitched { from, to, reason });
        }
        Ok(AutoSwitch::Exhausted { reason }) => {
            let _ = app.emit("auto-switch-exhausted", reason);
        }
        Ok(AutoSwitch::Idle) => {}
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

/// Keep every saved account's token alive.
///
/// Claude Code renews only the login it is running under, and only while it is
/// running: without this, a stored account's token expires a few hours after
/// the last switch away from it, its meters go blank, and the auto-switch is
/// left choosing between accounts it cannot read. Most passes do nothing but
/// read a handful of expiry dates.
async fn keep_tokens_fresh(app: AppHandle) {
    // A failure repeats every pass; the user needs to hear it once.
    let mut reported: std::collections::HashSet<String> = std::collections::HashSet::new();

    loop {
        let outcomes = oauth::renew_due(oauth::LIVE_MARGIN_MS, oauth::STORED_MARGIN_MS).await;

        for outcome in &outcomes {
            match outcome.status {
                oauth::Status::Failed => {
                    if reported.insert(outcome.id.clone()) {
                        eprintln!(
                            "token renewal for {}: {}",
                            outcome.id,
                            outcome.error.as_deref().unwrap_or("failed")
                        );
                        let _ = app.emit("token-refresh-failed", outcome.clone());
                    }
                }
                // Deferred says nothing yet — the next pass is seconds away.
                oauth::Status::Deferred => {}
                _ => {
                    reported.remove(&outcome.id);
                }
            }
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
        .manage(Cache::load())
        .manage(update::Latest::default())
        .manage(StartupPick::default())
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
