//! Reads and writes the login Claude Code keeps for itself.
//!
//! On Linux that is two files under the home directory. On macOS the tokens
//! live in the login Keychain instead, under the service Claude Code uses, and
//! only `~/.claude.json` stays on disk — so the credential half of this module
//! goes through a small backend switch while the config half does not.

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Keys in `~/.claude.json` that identify the logged-in account. Everything
/// else in that file (projects, history, tips) is machine state and must
/// survive a switch untouched.
const ACCOUNT_KEYS: &[&str] = &[
    "oauthAccount",
    "userID",
    "organizationUuid",
    "customApiKeyResponses",
    "subscriptionNoticeCount",
    "hasAvailableSubscription",
    "hasAvailableMaxSubscription",
    "isQualifiedForDataSharing",
];

pub fn home() -> Result<PathBuf> {
    dirs::home_dir().ok_or_else(|| anyhow!("cannot resolve the home directory"))
}

pub fn credentials_path() -> Result<PathBuf> {
    Ok(home()?.join(".claude").join(".credentials.json"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(home()?.join(".claude.json"))
}

fn read_json(path: &PathBuf) -> Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(
            serde_json::from_str(&text)
                .with_context(|| format!("{} is not valid JSON", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Write through a sibling temp file so a crash mid-write cannot truncate the
/// original — `~/.claude.json` holds unrecoverable session history.
fn write_json_atomic(path: &PathBuf, value: &Value, private: bool) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("cswitch-tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    if private {
        set_private(&tmp)?;
    }
    fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn set_private(path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private(_path: &PathBuf) -> Result<()> {
    Ok(())
}

fn remove_credentials_file() -> Result<()> {
    match fs::remove_file(credentials_path()?) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn read_credentials() -> Result<Option<Value>> {
    read_json(&credentials_path()?)
}

#[cfg(not(target_os = "macos"))]
fn write_credentials_unlocked(value: &Value) -> Result<()> {
    write_json_atomic(&credentials_path()?, value, true)
}

#[cfg(not(target_os = "macos"))]
fn remove_credentials_unlocked() -> Result<()> {
    remove_credentials_file()
}

/// Where a macOS install actually keeps its tokens.
///
/// Recent Claude Code uses the Keychain, older ones (and installs where it is
/// unavailable) use the same file as on Linux. Whichever one currently holds a
/// login is the one written back to, so a switch never leaves the CLI reading a
/// stale copy from the other.
#[cfg(target_os = "macos")]
fn credential_backend() -> Backend {
    if keychain::read().unwrap_or(None).is_some() {
        Backend::Keychain
    } else if credentials_path().map(|p| p.exists()).unwrap_or(false) {
        Backend::File
    } else {
        Backend::Keychain
    }
}

#[cfg(target_os = "macos")]
enum Backend {
    Keychain,
    File,
}

#[cfg(target_os = "macos")]
pub fn read_credentials() -> Result<Option<Value>> {
    match keychain::read()? {
        Some(v) => Ok(Some(v)),
        None => read_json(&credentials_path()?),
    }
}

#[cfg(target_os = "macos")]
fn write_credentials_unlocked(value: &Value) -> Result<()> {
    match credential_backend() {
        Backend::Keychain => keychain::write(value),
        Backend::File => write_json_atomic(&credentials_path()?, value, true),
    }
}

#[cfg(target_os = "macos")]
fn remove_credentials_unlocked() -> Result<()> {
    // Both, unconditionally: a logout that leaves a copy behind in the store it
    // did not pick would silently log the user back in.
    keychain::remove()?;
    remove_credentials_file()
}

/// The macOS login Keychain, as Claude Code uses it.
#[cfg(target_os = "macos")]
mod keychain {
    use anyhow::{Context, Result};
    use security_framework::passwords::{
        delete_generic_password, get_generic_password, set_generic_password,
    };
    use serde_json::Value;

    /// The service name Claude Code stores its credentials under. Matching it
    /// exactly is what makes the switch visible to the CLI.
    const SERVICE: &str = "Claude Code-credentials";

    /// Keychain items are per-account; Claude Code uses the short user name.
    fn account() -> String {
        std::env::var("USER")
            .ok()
            .filter(|u| !u.is_empty())
            .or_else(|| {
                dirs::home_dir()
                    .and_then(|h| h.file_name().map(|n| n.to_string_lossy().to_string()))
            })
            .unwrap_or_default()
    }

    /// `None` covers both "no item" and "the item is not readable": either way
    /// there is no login here, and the file backend is tried next.
    pub fn read() -> Result<Option<Value>> {
        let Ok(bytes) = get_generic_password(SERVICE, &account()) else {
            return Ok(None);
        };
        let text = String::from_utf8_lossy(&bytes);
        match serde_json::from_str(&text) {
            Ok(value) => Ok(Some(value)),
            // A malformed item is not something to fail the whole app over.
            Err(_) => Ok(None),
        }
    }

    pub fn write(value: &Value) -> Result<()> {
        let bytes = serde_json::to_vec(value)?;
        set_generic_password(SERVICE, &account(), &bytes)
            .context("cannot write the login to the Keychain")
    }

    pub fn remove() -> Result<()> {
        // Absent is the desired end state, so a failed delete is not an error.
        let _ = delete_generic_password(SERVICE, &account());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Writing next to a running session
// ---------------------------------------------------------------------------

/// How long to keep trying for the lock before giving up on this attempt.
/// Claude Code holds it only for the milliseconds a write takes.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// A lock whose mtime has stopped moving belongs to a process that is gone.
/// Claude Code's own threshold — and the reason a session killed mid-refresh
/// leaves the CLI saying "another Claude Code process is refreshing it" until
/// something clears the directory it left behind.
const LOCK_STALE: Duration = Duration::from_secs(15);

/// The lock Claude Code takes before writing its login is `proper-lockfile`'s,
/// so this one is too: a *directory* next to the file it guards, where `mkdir`
/// is the atom that decides who holds it. Matching the protocol exactly is the
/// whole point — the app and a running session then take turns instead of
/// landing on each other, whichever of the two gets there first.
fn lock_dir() -> Result<PathBuf> {
    Ok(home()?.join(".claude").join(".storage-write.lock"))
}

/// Held for as long as the guard lives, released when it drops.
pub struct WriteLock {
    dir: PathBuf,
}

impl Drop for WriteLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.dir);
    }
}

/// Someone else is mid-write. Not a failure — a reason to come back, which is
/// what the token keeper does a few seconds later.
#[derive(Debug)]
pub struct Busy;

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", crate::i18n::t("errors.login_busy"))
    }
}

impl std::error::Error for Busy {}

fn lock_is_stale(dir: &Path, after: Duration) -> bool {
    fs::metadata(dir)
        .and_then(|m| m.modified())
        .map(|mtime| mtime.elapsed().map(|age| age > after).unwrap_or(false))
        .unwrap_or(false)
}

fn try_lock(dir: &PathBuf, stale_after: Duration) -> Result<bool> {
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::create_dir(dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if !lock_is_stale(dir, stale_after) {
                return Ok(false);
            }
            // Exactly what the CLI does with a lock nobody came back for, and
            // the one thing that unsticks a session killed mid-refresh.
            let _ = fs::remove_dir(dir);
            Ok(fs::create_dir(dir).is_ok())
        }
        Err(e) => Err(e).context("cannot take the login write lock"),
    }
}

/// Take the lock, backing off the way the CLI does rather than spinning.
pub fn lock_writes(wait: Duration) -> Result<WriteLock> {
    let dir = lock_dir()?;
    let deadline = Instant::now() + wait;
    let mut delay = Duration::from_millis(100);
    loop {
        if try_lock(&dir, LOCK_STALE)? {
            return Ok(WriteLock { dir });
        }
        if Instant::now() >= deadline {
            return Err(Busy.into());
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_secs(1));
    }
}

/// Read-modify-write the live login while holding the lock.
///
/// `update` is handed the credentials as they are *inside* the lock, which need
/// not be what the caller last read: a session may have rotated them in the
/// meantime, and returning `None` is how the caller stands down when it did.
pub fn update_credentials<F>(wait: Duration, update: F) -> Result<Option<Value>>
where
    F: FnOnce(Option<Value>) -> Result<Option<Value>>,
{
    let _lock = lock_writes(wait)?;
    let Some(next) = update(read_credentials()?)? else {
        return Ok(None);
    };
    write_credentials_unlocked(&next)?;
    Ok(Some(next))
}

/// Replace the live login. Locked, because a switch that lands in the middle of
/// a session's own write is how one of the two ends up half applied.
pub fn write_credentials(value: &Value) -> Result<()> {
    let _lock = lock_writes(LOCK_WAIT)?;
    write_credentials_unlocked(value)
}

pub fn remove_credentials() -> Result<()> {
    let _lock = lock_writes(LOCK_WAIT)?;
    remove_credentials_unlocked()
}

pub fn read_config() -> Result<Value> {
    Ok(read_json(&config_path()?)?.unwrap_or_else(|| Value::Object(Map::new())))
}

/// The account-identifying slice of `~/.claude.json`.
pub fn read_account_keys() -> Result<Value> {
    let config = read_config()?;
    let mut out = Map::new();
    if let Some(obj) = config.as_object() {
        for key in ACCOUNT_KEYS {
            if let Some(v) = obj.get(*key) {
                out.insert((*key).to_string(), v.clone());
            }
        }
    }
    Ok(Value::Object(out))
}

/// Replace the account slice of `~/.claude.json`, leaving every other key as it
/// is. Keys absent from `account` are removed so a stale identity cannot linger.
pub fn write_account_keys(account: &Value) -> Result<()> {
    let path = config_path()?;
    let mut config = read_config()?;
    let obj = config
        .as_object_mut()
        .ok_or_else(|| anyhow!("~/.claude.json is not a JSON object"))?;

    let incoming = account.as_object().cloned().unwrap_or_default();
    for key in ACCOUNT_KEYS {
        match incoming.get(*key) {
            Some(v) => {
                obj.insert((*key).to_string(), v.clone());
            }
            None => {
                obj.remove(*key);
            }
        }
    }
    write_json_atomic(&path, &config, false)
}

pub fn email_of(account: &Value) -> Option<String> {
    account
        .get("oauthAccount")?
        .get("emailAddress")?
        .as_str()
        .map(str::to_string)
}

pub fn subscription_of(credentials: &Value) -> Option<String> {
    credentials
        .get("claudeAiOauth")?
        .get("subscriptionType")?
        .as_str()
        .map(str::to_string)
}

/// Expiry of the OAuth access token, epoch milliseconds.
pub fn expires_at_of(credentials: &Value) -> Option<u64> {
    credentials
        .get("claudeAiOauth")?
        .get("expiresAt")?
        .as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of our own: these tests exercise the protocol, and the real
    /// lock sits next to a login nobody should be poking at from a test.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("claude-switch-{name}-{}.lock", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_free_lock_is_taken() {
        let dir = scratch("free");
        assert!(try_lock(&dir, LOCK_STALE).expect("no error"));
        assert!(dir.exists());
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn a_lock_someone_is_holding_is_refused() {
        let dir = scratch("held");
        fs::create_dir_all(&dir).expect("held by someone else");
        assert!(!try_lock(&dir, LOCK_STALE).expect("no error"));
        let _ = fs::remove_dir(&dir);
    }

    /// The state a session killed mid-refresh leaves behind: the directory is
    /// there, its owner is not. Claude Code stops being able to renew until
    /// somebody clears it, so this app does.
    #[test]
    fn an_abandoned_lock_is_cleared_and_taken() {
        let dir = scratch("abandoned");
        fs::create_dir_all(&dir).expect("left behind");
        // Any age at all counts as abandoned here, which is the branch under
        // test; in the app the threshold is Claude Code's own fifteen seconds.
        assert!(try_lock(&dir, Duration::ZERO).expect("no error"));
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn the_guard_releases_on_drop() {
        let dir = scratch("guard");
        assert!(try_lock(&dir, LOCK_STALE).expect("no error"));
        {
            let _guard = WriteLock { dir: dir.clone() };
        }
        assert!(!dir.exists(), "dropping the guard removes the lock");
    }
}
