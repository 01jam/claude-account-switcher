//! Where a message the user did not ask for goes.
//!
//! Almost everything this app has to say happens while nobody is looking at it:
//! the window spends its life hidden in the tray, so an auto-switch at three in
//! the afternoon announced in a strip above the account list is read at six, if
//! at all. So the desktop's own notification service gets it when the window is
//! not up, and the window gets it — as a toast over the list — when it is.
//! Never both: the same sentence arriving twice reads as a bug.
//!
//! The text is rendered here rather than in the webview because a notification
//! has no webview to render it in, and one rendering path is what keeps the two
//! channels from saying different things. The cost is that a message already
//! written does not follow a change of language; for something that lives a few
//! seconds that is not worth a second path.

use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

/// How loudly the window draws it. The desktop service has no equivalent knob,
/// so this only ever reaches the toast.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Level {
    Info,
    Error,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub text: String,
    pub level: Level,
}

/// Messages that reached neither channel, kept for the next window.
///
/// The desktop service is allowed to be absent — a Linux session running no
/// notification daemon, a macOS install where the user said no — and the way it
/// says so is by failing, quietly, without telling anyone. Something has to hold
/// the message, or "the app moved you to another account overnight" is lost.
#[derive(Default)]
pub struct Pending(Mutex<Vec<Notice>>);

/// Past this it is history rather than news, and a window opening onto a stack
/// of them is something to dismiss rather than something to read.
const MAX_PENDING: usize = 5;

impl Pending {
    fn push(&self, notice: Notice) {
        if let Ok(mut held) = self.0.lock() {
            if held.len() >= MAX_PENDING {
                held.remove(0);
            }
            held.push(notice);
        }
    }

    /// Handed over once: the window that asks is the one that shows them.
    pub fn take(&self) -> Vec<Notice> {
        self.0
            .lock()
            .map(|mut held| std::mem::take(&mut *held))
            .unwrap_or_default()
    }
}

pub fn info(app: &AppHandle, text: String) {
    announce(app, text, Level::Info);
}

pub fn error(app: &AppHandle, text: String) {
    announce(app, text, Level::Error);
}

fn announce(app: &AppHandle, text: String, level: Level) {
    let notice = Notice { text, level };
    if window_is_up(app) {
        let _ = app.emit("notice", notice);
    } else if !to_desktop(app, &notice) {
        app.state::<Pending>().push(notice);
    }
}

/// Visible *and* in front. A window sitting behind the editor is as unread as
/// one in the tray, and where the user's eyes are is the whole question here.
fn window_is_up(app: &AppHandle) -> bool {
    app.get_webview_window("main").is_some_and(|window| {
        window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false)
    })
}

/// True when the desktop took it. False is not worth surfacing on its own: it
/// means this machine has nowhere to put a notification, and the caller falls
/// back to holding the message for the window.
///
/// No title is set — the builder falls back to the product name, which is what
/// every notification service puts in that line anyway.
fn to_desktop(app: &AppHandle, notice: &Notice) -> bool {
    match app.notification().builder().body(&notice.text).show() {
        Ok(()) => true,
        Err(e) => {
            eprintln!("notification: {e}");
            false
        }
    }
}
