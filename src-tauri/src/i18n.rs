//! The strings the backend owns — the tray menu, the auto-switch notice, and
//! the errors that reach the user verbatim.
//!
//! Both halves of the app read the same `locales/*.yml` files: the window's text
//! is compiled into the bundle by Vite, and these copies are embedded here. The
//! keys are the YAML paths joined with dots (`tray.quit`), and `{name}`
//! placeholders are filled by `t_args`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use crate::store;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    It,
    En,
}

/// Every language the app ships, with the file it is built from. Adding one is
/// a line here plus the matching entry in `src/i18n.ts`.
const CATALOGS: &[(Lang, &str)] = &[
    (Lang::It, include_str!("../../locales/it.yml")),
    (Lang::En, include_str!("../../locales/en.yml")),
];

impl Lang {
    pub fn tag(self) -> &'static str {
        match self {
            Lang::It => "it",
            Lang::En => "en",
        }
    }

    /// Matches on the primary subtag, so `it-CH` counts as Italian. Anything
    /// unknown falls back to English.
    pub fn from_tag(tag: &str) -> Lang {
        if tag.to_ascii_lowercase().starts_with("it") {
            Lang::It
        } else {
            Lang::En
        }
    }
}

/// The system's language as a tag like `it-IT`. Also handed to the settings
/// dialog, so "Automatic" can name the language it currently resolves to.
pub fn system_tag() -> String {
    sys_locale::get_locale().unwrap_or_else(|| "en".to_string())
}

/// Resolved language cache: 0 unresolved, 1 Italian, 2 English. Every tray
/// rebuild asks for strings, and resolving means reading `state.json`.
static CURRENT: AtomicU8 = AtomicU8::new(0);

pub fn lang() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::It,
        2 => Lang::En,
        _ => {
            let lang = match store::language().ok().flatten() {
                Some(tag) => Lang::from_tag(&tag),
                None => Lang::from_tag(&system_tag()),
            };
            CURRENT.store(if lang == Lang::It { 1 } else { 2 }, Ordering::Relaxed);
            lang
        }
    }
}

/// Call after the stored override changes, so the tray picks the new language
/// up on its next rebuild.
pub fn invalidate() {
    CURRENT.store(0, Ordering::Relaxed);
}

type Catalog = HashMap<String, String>;

fn catalogs() -> &'static HashMap<&'static str, Catalog> {
    static CATALOG_MAP: OnceLock<HashMap<&'static str, Catalog>> = OnceLock::new();
    CATALOG_MAP.get_or_init(|| {
        CATALOGS
            .iter()
            .map(|(lang, source)| (lang.tag(), parse(lang.tag(), source)))
            .collect()
    })
}

/// Flatten the nested YAML into dotted keys. A file that fails to parse yields
/// an empty catalog, and `t` then shows the key — visible, but not fatal.
fn parse(tag: &str, source: &str) -> Catalog {
    let mut out = Catalog::new();
    let root = match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(source) {
        Ok(root) => root,
        Err(e) => {
            eprintln!("i18n: locales/{tag}.yml could not be parsed: {e}");
            return out;
        }
    };
    flatten(&root, String::new(), &mut out);
    out
}

fn flatten(value: &serde_yaml_ng::Value, prefix: String, out: &mut Catalog) {
    match value {
        serde_yaml_ng::Value::Mapping(map) => {
            for (key, child) in map {
                let Some(key) = key.as_str() else { continue };
                let path = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten(child, path, out);
            }
        }
        serde_yaml_ng::Value::String(s) => {
            out.insert(prefix, s.clone());
        }
        _ => {}
    }
}

/// The string for `key` in the current language, falling back to English and
/// then to the key itself.
pub fn t(key: &str) -> String {
    let all = catalogs();
    all.get(lang().tag())
        .and_then(|c| c.get(key))
        .or_else(|| all.get(Lang::En.tag()).and_then(|c| c.get(key)))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

/// `t` with `{name}` placeholders replaced. Unknown placeholders are left in
/// place rather than blanked, so a mismatch is visible in the UI.
pub fn t_args(key: &str, args: &[(&str, &str)]) -> String {
    let mut text = t(key);
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key translated in one file and forgotten in another shows up as
    /// English text in an Italian window; catching it here is cheaper.
    #[test]
    fn every_catalog_has_the_same_keys() {
        let all = catalogs();
        let reference = &all[Lang::En.tag()];
        assert!(!reference.is_empty(), "the English catalog failed to parse");
        for (tag, catalog) in all {
            let mut missing: Vec<_> = reference
                .keys()
                .filter(|k| !catalog.contains_key(*k))
                .collect();
            let mut extra: Vec<_> = catalog
                .keys()
                .filter(|k| !reference.contains_key(*k))
                .collect();
            missing.sort();
            extra.sort();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "{tag}: missing {missing:?}, unexpected {extra:?}"
            );
        }
    }
}
