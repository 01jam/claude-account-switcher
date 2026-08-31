//! Whether a newer release is out, and getting its package onto the machine.
//!
//! Installing is deliberately not part of this. Every format this project ships
//! needs either root or a gesture the user has to make themselves: a `.deb` or
//! `.rpm` goes through the system's own installer, a `.dmg` gets dragged. An
//! app that asked for a password to write outside its own directory would be
//! asking for more trust than a version check is worth. So this fetches the
//! file and hands it to the desktop, which is the last point where it can help
//! without holding something it has no business holding.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::Duration;

use crate::i18n;

/// Where the releases are. Hard-coded on purpose: an app that took this from a
/// config file would be one prompt away from installing a package of someone
/// else's choosing.
const LATEST_URL: &str =
    "https://api.github.com/repos/01jam/claude-account-switcher/releases/latest";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The API refuses a request without one, and a version here is what turns a
/// rate-limit complaint into something traceable.
const USER_AGENT: &str = concat!("claude-account-switcher/", env!("CARGO_PKG_VERSION"));

/// A downloaded package may be large; the check itself never is.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// What the API answers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize, Clone)]
struct Asset {
    name: String,
    browser_download_url: String,
}

// ---------------------------------------------------------------------------
// What the window is told
// ---------------------------------------------------------------------------

/// A release newer than the running app, narrowed to the one file this machine
/// can actually use out of the several every release carries.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Available {
    /// Without the tag's leading `v`: this is shown, not compared.
    pub version: String,
    pub notes_url: String,
    /// `None` when the release carries nothing in this machine's format. That
    /// is still an update worth announcing — it just has to be fetched by hand,
    /// and saying so beats a button that quietly downloads the wrong thing.
    pub asset_name: Option<String>,
    pub asset_url: Option<String>,
}

/// What the window asks for when it opens: which version is running, and
/// whether a newer one is out. The two belong together — "0.2.3 is available"
/// means nothing to someone who does not know what they are on.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub current: String,
    pub available: Option<Available>,
}

pub fn running_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The last answer the check got, for a window that opened after it ran.
#[derive(Default)]
pub struct Latest(Mutex<Option<Available>>);

impl Latest {
    pub fn get(&self) -> Option<Available> {
        self.0.lock().ok().and_then(|l| l.clone())
    }

    /// Returns whether this is news. The check runs for as long as the app
    /// does; without this the same version would be announced every few hours.
    pub fn set(&self, available: Option<Available>) -> bool {
        match self.0.lock() {
            Ok(mut slot) if *slot != available => {
                *slot = available;
                true
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Versions
// ---------------------------------------------------------------------------

/// Three numbers, and nothing else accepted.
///
/// Tags carry a leading `v`. Anything past the patch number — a `-beta.1` — is
/// not something this project publishes, and refusing to parse it is safer than
/// inventing an ordering: an unparseable tag simply never counts as newer.
fn parse_version(tag: &str) -> Option<(u64, u64, u64)> {
    let mut parts = tag.trim().trim_start_matches('v').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

fn is_newer(tag: &str, current: &str) -> bool {
    match (parse_version(tag), parse_version(current)) {
        (Some(theirs), Some(ours)) => theirs > ours,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Picking the file
// ---------------------------------------------------------------------------

/// The package format this installation arrived in, and so the only one an
/// update is any use in.
#[cfg(target_os = "macos")]
fn wanted_suffix() -> &'static str {
    ".dmg"
}

/// An AppImage says what it is through the environment its runtime sets. A
/// packaged install has to be guessed at from the distribution — which is as
/// close as this gets without interrogating dpkg and rpm in turn, and picking
/// the wrong one costs a download the user can throw away, not a broken install.
#[cfg(not(target_os = "macos"))]
fn wanted_suffix() -> &'static str {
    if std::env::var_os("APPIMAGE").is_some() {
        ".AppImage"
    } else if std::path::Path::new("/etc/debian_version").exists() {
        ".deb"
    } else {
        ".rpm"
    }
}

fn pick_asset<'a>(assets: &'a [Asset], suffix: &str) -> Option<&'a Asset> {
    assets.iter().find(|a| a.name.ends_with(suffix))
}

// ---------------------------------------------------------------------------
// Checking
// ---------------------------------------------------------------------------

fn client(timeout: Duration) -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().timeout(timeout).build()?)
}

/// Ask GitHub for the newest release. `Ok(None)` means this app is current.
pub async fn check() -> Result<Option<Available>> {
    check_against(running_version()).await
}

/// The version to compare against is a parameter so that the whole path — the
/// request, the shape the API answers in, the asset names — can be exercised
/// against the real release without waiting for one to come out.
async fn check_against(current: &str) -> Result<Option<Available>> {
    let response = client(REQUEST_TIMEOUT)?
        .get(LATEST_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!(i18n::t_args(
            "errors.api_status",
            &[("status", &status.as_u16().to_string())]
        )));
    }

    let release: Release = response.json().await?;
    if !is_newer(&release.tag_name, current) {
        return Ok(None);
    }

    let asset = pick_asset(&release.assets, wanted_suffix());
    Ok(Some(Available {
        version: release.tag_name.trim_start_matches('v').to_string(),
        notes_url: release.html_url,
        asset_name: asset.map(|a| a.name.clone()),
        asset_url: asset.map(|a| a.browser_download_url.clone()),
    }))
}

// ---------------------------------------------------------------------------
// Fetching
// ---------------------------------------------------------------------------

/// Download the package and hand it to the desktop. Returns what the user
/// should now be looking at — a file name, or the release page when there was
/// nothing here to download.
pub async fn fetch_and_open(available: &Available) -> Result<String> {
    let (Some(name), Some(url)) = (&available.asset_name, &available.asset_url) else {
        // Nothing in this machine's format. The release page is the honest
        // answer, and a browser is better at it than this app is.
        open_with_desktop(&available.notes_url)?;
        return Ok(available.notes_url.clone());
    };

    let bytes = client(DOWNLOAD_TIMEOUT)?
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    // Downloads, not a cache directory: the user is about to be handed a
    // password prompt by the installer, and the file it names should be
    // somewhere they can find, check and delete.
    let dir = dirs::download_dir().unwrap_or_else(std::env::temp_dir);
    let path = dir.join(name);

    let target = path.clone();
    tauri::async_runtime::spawn_blocking(move || std::fs::write(&target, &bytes)).await??;

    open_with_desktop(&path.to_string_lossy())?;
    Ok(name.clone())
}

/// Open the release page in the browser. Reading what changed before installing
/// it is the ordinary thing to want, and the notes are not ours to reproduce.
pub fn open_notes(available: &Available) -> Result<()> {
    open_with_desktop(&available.notes_url)
}

/// Hand a file or URL to whatever the desktop opens it with — the package
/// installer, or the browser. This is where the app stops and the system's own
/// confirmation begins.
fn open_with_desktop(target: &str) -> Result<()> {
    use std::process::Command;

    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(not(target_os = "macos"))]
    let opener = "xdg-open";

    Command::new(opener)
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|e| anyhow!(i18n::t_args("errors.no_opener", &[("error", &e.to_string())])))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn assets(names: &[&str]) -> Vec<Asset> {
        names
            .iter()
            .map(|n| Asset {
                name: (*n).to_string(),
                browser_download_url: format!("https://example.invalid/{n}"),
            })
            .collect()
    }

    #[test]
    fn a_tag_is_read_with_or_without_its_v() {
        assert_eq!(parse_version("v0.2.2"), Some((0, 2, 2)));
        assert_eq!(parse_version("0.2.2"), Some((0, 2, 2)));
        assert_eq!(parse_version(" v1.10.0 "), Some((1, 10, 0)));
    }

    /// Ordering is numeric, not textual: 10 comes after 9, and a string compare
    /// would have said otherwise.
    #[test]
    fn a_later_version_wins_on_numbers() {
        assert!(is_newer("v0.10.0", "0.9.0"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(is_newer("v0.2.3", "0.2.2"));
    }

    #[test]
    fn the_running_version_is_not_an_update() {
        assert!(!is_newer("v0.2.2", "0.2.2"));
        assert!(!is_newer("v0.2.1", "0.2.2"));
    }

    /// A tag this project does not publish is never offered. Guessing where a
    /// prerelease sorts is how an app talks someone into a downgrade.
    #[test]
    fn an_unreadable_tag_is_never_newer() {
        assert!(!is_newer("v0.3.0-beta.1", "0.2.2"));
        assert!(!is_newer("nightly", "0.2.2"));
        assert!(!is_newer("v0.3", "0.2.2"));
        assert!(!is_newer("v0.3.0.1", "0.2.2"));
    }

    #[test]
    fn the_asset_is_chosen_by_this_machine_s_format() {
        let release = assets(&[
            "claude-account-switcher-0.2.2-1.x86_64.rpm",
            "claude-account-switcher_0.2.2_amd64.AppImage",
            "claude-account-switcher_0.2.2_amd64.deb",
            "claude-account-switcher_0.2.2_universal.dmg",
            "SHA256SUMS",
        ]);

        for (suffix, expected) in [
            (".deb", "claude-account-switcher_0.2.2_amd64.deb"),
            (".rpm", "claude-account-switcher-0.2.2-1.x86_64.rpm"),
            (".AppImage", "claude-account-switcher_0.2.2_amd64.AppImage"),
            (".dmg", "claude-account-switcher_0.2.2_universal.dmg"),
        ] {
            assert_eq!(pick_asset(&release, suffix).map(|a| a.name.as_str()), Some(expected));
        }
    }

    /// A release built before this platform was supported still gets announced;
    /// it just has no button to press.
    #[test]
    fn a_release_without_this_format_picks_nothing() {
        assert!(pick_asset(&assets(&["SHA256SUMS"]), ".deb").is_none());
    }

    /// Ignored by default because it talks to GitHub, and a test suite that
    /// needs the network is one that fails on a train. Run it by hand — `cargo
    /// test -- --ignored` — when the endpoint or the release layout might have
    /// moved: it is the only thing here that checks the names this machine
    /// looks for against the names a release actually carries.
    #[test]
    #[ignore = "talks to GitHub"]
    fn the_published_release_parses_and_carries_a_package_for_this_machine() {
        let found = tauri::async_runtime::block_on(check_against("0.0.1"))
            .expect("the releases endpoint answered");
        let available = found.expect("every release is newer than 0.0.1");

        assert!(parse_version(&available.version).is_some(), "{available:?}");
        assert!(available.notes_url.starts_with("https://github.com/"));
        assert!(
            available.asset_name.is_some(),
            "no {} in the latest release: {available:?}",
            wanted_suffix()
        );
    }
}
