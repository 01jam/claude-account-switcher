//! On-disk store of saved accounts, under `~/.config/claude-switch`.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Above this share of a limit window the account is considered spent and the
/// auto-switch moves on. 100 means "only when the window is actually full".
pub const DEFAULT_THRESHOLD: f64 = 100.0;

fn default_threshold() -> f64 {
    DEFAULT_THRESHOLD
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub label: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub subscription: Option<String>,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub last_used: Option<u64>,
    #[serde(default)]
    pub expires_at: Option<u64>,
    /// Switch away once the 5-hour window passes this percentage.
    #[serde(default = "default_threshold")]
    pub five_hour_threshold: f64,
    /// Switch away once the 7-day window passes this percentage.
    #[serde(default = "default_threshold")]
    pub seven_day_threshold: f64,
    /// Position in the user-defined rotation. Absent means "not sorted yet".
    #[serde(default)]
    pub order: Option<u32>,
}

/// A saved account as the UI sees it.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    pub subscription: Option<String>,
    pub created_at: u64,
    pub last_used: Option<u64>,
    pub expires_at: Option<u64>,
    pub five_hour_threshold: f64,
    pub seven_day_threshold: f64,
    pub active: bool,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct State {
    active: Option<String>,
    #[serde(default)]
    auto_switch: bool,
    /// Start with the window hidden, leaving only the tray icon.
    #[serde(default)]
    start_hidden: bool,
    /// UI language override as a tag (`it`, `en`). Absent means "follow the
    /// system", which is what a fresh install does.
    #[serde(default)]
    language: Option<String>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn root() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("cannot resolve the config directory"))?
        .join("claude-switch");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn profiles_dir() -> Result<PathBuf> {
    let dir = root()?.join("profiles");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn profile_dir(id: &str) -> Result<PathBuf> {
    Ok(profiles_dir()?.join(sanitize_id(id)?))
}

/// Profile ids come from the frontend and become path segments, so reject
/// anything that could escape the profiles directory.
fn sanitize_id(id: &str) -> Result<String> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(id.to_string())
    } else {
        Err(anyhow!("invalid profile id: {id}"))
    }
}

pub fn slugify(label: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    let s: String = s.chars().take(48).collect();
    if s.is_empty() {
        format!("account-{}", now_ms())
    } else {
        s
    }
}

pub fn unique_id(label: &str) -> Result<String> {
    let base = slugify(label);
    let mut candidate = base.clone();
    let mut n = 2;
    while profile_dir(&candidate)?.exists() {
        candidate = format!("{base}-{n}");
        n += 1;
    }
    Ok(candidate)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T> {
    let text = fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn write_json<T: Serialize>(path: &PathBuf, value: &T, private: bool) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    if private {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

pub fn save(id: &str, meta: &Meta, credentials: &Value, account: &Value) -> Result<()> {
    let dir = profile_dir(id)?;
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    write_json(&dir.join("meta.json"), meta, false)?;
    write_json(&dir.join("credentials.json"), credentials, true)?;
    write_json(&dir.join("account.json"), account, false)?;
    Ok(())
}

pub fn save_meta(id: &str, meta: &Meta) -> Result<()> {
    write_json(&profile_dir(id)?.join("meta.json"), meta, false)
}

pub fn load_meta(id: &str) -> Result<Meta> {
    read_json(&profile_dir(id)?.join("meta.json"))
}

pub fn load_credentials(id: &str) -> Result<Value> {
    read_json(&profile_dir(id)?.join("credentials.json"))
}

pub fn load_account(id: &str) -> Result<Value> {
    let path = profile_dir(id)?.join("account.json");
    if path.exists() {
        read_json(&path)
    } else {
        Ok(Value::Object(Default::default()))
    }
}

pub fn exists(id: &str) -> bool {
    profile_dir(id).map(|p| p.exists()).unwrap_or(false)
}

pub fn delete(id: &str) -> Result<()> {
    let dir = profile_dir(id)?;
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    if active()? .as_deref() == Some(id) {
        set_active(None)?;
    }
    Ok(())
}

pub fn list() -> Result<Vec<Profile>> {
    let active = active()?;
    let mut out = Vec::new();
    for entry in fs::read_dir(profiles_dir()?)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        let Ok(meta) = load_meta(&id) else { continue };
        let order = meta.order;
        out.push((
            order,
            Profile {
                active: active.as_deref() == Some(id.as_str()),
                id,
                label: meta.label,
                email: meta.email,
                subscription: meta.subscription,
                created_at: meta.created_at,
                last_used: meta.last_used,
                expires_at: meta.expires_at,
                five_hour_threshold: meta.five_hour_threshold,
                seven_day_threshold: meta.seven_day_threshold,
            },
        ));
    }
    // The rotation order is what the user dragged; anything never sorted falls
    // to the end, alphabetically, so new accounts land predictably.
    out.sort_by(|(a_order, a), (b_order, b)| {
        a_order
            .unwrap_or(u32::MAX)
            .cmp(&b_order.unwrap_or(u32::MAX))
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    Ok(out.into_iter().map(|(_, p)| p).collect())
}

/// Persist the rotation order given as a full list of ids, best effort: ids that
/// no longer exist are skipped, and accounts missing from the list keep their
/// place at the end.
pub fn reorder(ids: &[String]) -> Result<()> {
    for (index, id) in ids.iter().enumerate() {
        let Ok(mut meta) = load_meta(id) else { continue };
        meta.order = Some(index as u32);
        save_meta(id, &meta)?;
    }
    Ok(())
}

pub fn set_thresholds(id: &str, five_hour: f64, seven_day: f64) -> Result<()> {
    let mut meta = load_meta(id)?;
    meta.five_hour_threshold = five_hour.clamp(1.0, 100.0);
    meta.seven_day_threshold = seven_day.clamp(1.0, 100.0);
    save_meta(id, &meta)
}

fn state_path() -> Result<PathBuf> {
    Ok(root()?.join("state.json"))
}

fn read_state() -> Result<State> {
    let path = state_path()?;
    if !path.exists() {
        return Ok(State::default());
    }
    Ok(read_json(&path).unwrap_or_default())
}

fn write_state(state: &State) -> Result<()> {
    write_json(&state_path()?, state, false)
}

pub fn active() -> Result<Option<String>> {
    Ok(read_state()?.active)
}

pub fn set_active(id: Option<&str>) -> Result<()> {
    let mut state = read_state()?;
    state.active = id.map(str::to_string);
    write_state(&state)
}

pub fn auto_switch() -> Result<bool> {
    Ok(read_state()?.auto_switch)
}

pub fn set_auto_switch(enabled: bool) -> Result<()> {
    let mut state = read_state()?;
    state.auto_switch = enabled;
    write_state(&state)
}

pub fn language() -> Result<Option<String>> {
    Ok(read_state()?.language)
}

pub fn set_language(tag: Option<&str>) -> Result<()> {
    let mut state = read_state()?;
    state.language = tag.map(str::to_string);
    write_state(&state)
}

pub fn start_hidden() -> Result<bool> {
    Ok(read_state()?.start_hidden)
}

pub fn set_start_hidden(enabled: bool) -> Result<()> {
    let mut state = read_state()?;
    state.start_hidden = enabled;
    write_state(&state)
}

/// Snapshot of whatever Claude Code currently has, kept outside the profiles so
/// an accidental overwrite is always recoverable.
pub fn backup(credentials: &Option<Value>, account: &Value) -> Result<()> {
    let dir = root()?.join("backups");
    fs::create_dir_all(&dir)?;
    let stamp = now_ms();
    if let Some(c) = credentials {
        write_json(&dir.join(format!("{stamp}.credentials.json")), c, true)?;
    }
    write_json(&dir.join(format!("{stamp}.account.json")), account, false)?;
    prune_backups(&dir, 20)?;
    Ok(())
}

fn prune_backups(dir: &PathBuf, keep: usize) -> Result<()> {
    let mut files: Vec<_> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .collect();
    files.sort();
    let excess = files.len().saturating_sub(keep * 2);
    for path in files.into_iter().take(excess) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}
