//! System tray icon and its account menu.

use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Manager, Wry,
};

use crate::i18n;
use crate::store::Profile;
use crate::usage::{self, Cache, Usage};
use crate::{actions, store};

pub const TRAY_ID: &str = "main";
const PROFILE_PREFIX: &str = "profile:";

/// A switch glyph rather than the app icon: in a panel full of app icons, what
/// this one does reads better than which app it is.
const TRAY_ICON: &[u8] = include_bytes!("../icons/tray.png");
/// Same glyph with a warning badge, for when an account is near its threshold.
const TRAY_ICON_ALERT: &[u8] = include_bytes!("../icons/tray-alert.png");

/// Menu items cannot carry an image on every Linux panel, so the warning rides
/// in the label as a character.
const WARN_MARK: &str = "\u{26a0} ";

/// Which of the two icons the tray is currently showing; starts as the plain
/// one, matching what `build` installs.
static ALERTING: AtomicBool = AtomicBool::new(false);

pub fn build(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::from_bytes(TRAY_ICON)?)
        .tooltip(i18n::t("app.name"))
        .menu(&menu(app)?)
        .on_menu_event(on_menu_event);

    // The macOS menu bar tints its items itself; handing it the glyph as a
    // template is what makes it follow light and dark mode.
    #[cfg(target_os = "macos")]
    let builder = builder.icon_as_template(true);

    builder.build(app)?;

    Ok(())
}

/// True when either window is within `WARN_MARGIN` of this account's threshold.
fn is_warning(profile: &Profile, usage: Option<&Usage>) -> bool {
    usage
        .and_then(|u| {
            u.approaching(
                profile.five_hour_threshold,
                profile.seven_day_threshold,
                usage::WARN_MARGIN,
            )
        })
        .is_some()
}

/// `Nome · 5h 92% · 7g 45%`, with the account's email when it adds something.
fn label_for(profile: &Profile, usage: Option<&Usage>) -> String {
    let mut parts = vec![match &profile.email {
        Some(email) if email != &profile.label => {
            format!("{}  ·  {}", profile.label, email)
        }
        _ => profile.label.clone(),
    }];

    if let Some(u) = usage {
        if let Some(w) = &u.five_hour {
            parts.push(format!(
                "{} {:.0}%",
                i18n::t("usage.five_hour_short"),
                w.utilization
            ));
        }
        if let Some(w) = &u.seven_day {
            parts.push(format!(
                "{} {:.0}%",
                i18n::t("usage.seven_day_short"),
                w.utilization
            ));
        }
    }

    let warn = if is_warning(profile, usage) {
        WARN_MARK
    } else {
        ""
    };
    format!("{warn}{}", parts.join("  ·  "))
}

fn menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::new(app)?;
    let profiles = store::list().unwrap_or_default();
    let usage = app.state::<Cache>().snapshot();

    if profiles.is_empty() {
        let empty = MenuItem::with_id(app, "noop", i18n::t("tray.no_accounts"), false, None::<&str>)?;
        menu.append(&empty)?;
    } else {
        for p in &profiles {
            let item = CheckMenuItem::with_id(
                app,
                format!("{PROFILE_PREFIX}{}", p.id),
                label_for(p, usage.get(&p.id)),
                true,
                p.active,
                None::<&str>,
            )?;
            menu.append(&item)?;
        }
    }

    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "open",
        i18n::t("tray.open_window"),
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        "quit",
        i18n::t("tray.quit"),
        true,
        None::<&str>,
    )?)?;

    Ok(menu)
}

/// Rebuild the menu so the checkmark and the account list stay in step with
/// the store after any change.
pub fn refresh(app: &AppHandle) {
    let Ok(new_menu) = menu(app) else { return };
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let _ = tray.set_menu(Some(new_menu));

    let profiles = store::list().unwrap_or_default();
    let usage = app.state::<Cache>().snapshot();
    let warnings: Vec<&Profile> = profiles
        .iter()
        .filter(|p| is_warning(p, usage.get(&p.id)))
        .collect();

    // The badged icon is the panel-level signal: something needs attention
    // without opening the menu. Only swapped when it actually changes — GTK
    // complains about redundant icon writes, and refresh runs often.
    let alerting = !warnings.is_empty();
    if ALERTING.swap(alerting, Ordering::Relaxed) != alerting {
        let bytes = if alerting { TRAY_ICON_ALERT } else { TRAY_ICON };
        if let Ok(image) = Image::from_bytes(bytes) {
            let _ = tray.set_icon(Some(image));
        }
    }

    let active = match store::active().ok().flatten() {
        Some(id) => store::load_meta(&id)
            .map(|m| format!("Claude · {}", m.email.unwrap_or(m.label)))
            .unwrap_or_else(|_| i18n::t("app.name")),
        None => i18n::t("tray.no_active_account"),
    };
    let tooltip = if warnings.is_empty() {
        active
    } else {
        let names: Vec<&str> = warnings.iter().map(|p| p.label.as_str()).collect();
        format!(
            "{active}\n{}: {}",
            i18n::t("tray.near_threshold"),
            names.join(", ")
        )
    };
    let _ = tray.set_tooltip(Some(tooltip));
}

fn on_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref().to_string();

    match id.as_str() {
        "quit" => app.exit(0),
        "open" => show_window(app),
        "noop" => {}
        _ => {
            if let Some(profile_id) = id.strip_prefix(PROFILE_PREFIX) {
                if let Err(e) = actions::switch_to(profile_id) {
                    eprintln!("switch failed: {e:#}");
                }
                crate::notify_changed(app);
            }
        }
    }
}

pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
