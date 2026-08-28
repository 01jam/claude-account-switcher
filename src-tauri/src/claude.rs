//! Reads and writes the login Claude Code keeps for itself.
//!
//! On Linux that is two files under the home directory. On macOS the tokens
//! live in the login Keychain instead, under the service Claude Code uses, and
//! only `~/.claude.json` stays on disk — so the credential half of this module
//! goes through a small backend switch while the config half does not.

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};
use std::fs;
use std::path::PathBuf;

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
pub fn write_credentials(value: &Value) -> Result<()> {
    write_json_atomic(&credentials_path()?, value, true)
}

#[cfg(not(target_os = "macos"))]
pub fn remove_credentials() -> Result<()> {
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
pub fn write_credentials(value: &Value) -> Result<()> {
    match credential_backend() {
        Backend::Keychain => keychain::write(value),
        Backend::File => write_json_atomic(&credentials_path()?, value, true),
    }
}

#[cfg(target_os = "macos")]
pub fn remove_credentials() -> Result<()> {
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
