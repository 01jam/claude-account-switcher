import { createContext, useContext } from "react";

import enMessages from "../locales/en.yml";
import itMessages from "../locales/it.yml";

/** Every language the window can render in. Adding one means a new
 *  `locales/<tag>.yml`, an entry here, and the matching line in
 *  `src-tauri/src/i18n.rs`. */
export const LANGS = ["it", "en"] as const;
export type Lang = (typeof LANGS)[number];

/** How each language names itself, for the settings dialog. Deliberately not in
 *  the catalogs: a language list reads best when every entry is in its own
 *  language, whatever the UI is currently set to. */
export const LANG_NAMES: Record<Lang, string> = {
  it: "Italiano",
  en: "English",
};

/** Flat `section.key` → text, matching how the Rust side reads the same file. */
type Catalog = Record<string, string>;

function flatten(node: unknown, prefix = "", out: Catalog = {}): Catalog {
  if (typeof node === "string") {
    out[prefix] = node;
  } else if (node && typeof node === "object") {
    for (const [key, child] of Object.entries(node)) {
      flatten(child, prefix ? `${prefix}.${key}` : key, out);
    }
  }
  return out;
}

const CATALOGS: Record<Lang, Catalog> = {
  it: flatten(itMessages),
  en: flatten(enMessages),
};

/** Matches on the primary subtag, so `it-CH` counts as Italian; anything else
 *  falls back to English. Mirrors `Lang::from_tag` in the backend. */
export function resolveLang(tag: string | null | undefined): Lang {
  return tag?.toLowerCase().startsWith("it") ? "it" : "en";
}

export type Translate = (
  key: string,
  args?: Record<string, string | number>,
) => string;

function translator(lang: Lang): Translate {
  return (key, args) => {
    const text = CATALOGS[lang][key] ?? CATALOGS.en[key];
    if (text === undefined) {
      // A key that exists in neither catalog is a typo at the call site; the
      // key itself is shown so it is obvious where to look.
      if (import.meta.env.DEV) console.warn(`i18n: no string for "${key}"`);
      return key;
    }
    if (!args) return text;
    return text.replace(/\{(\w+)\}/g, (whole, name: string) =>
      name in args ? String(args[name]) : whole,
    );
  };
}

// One translator per language, built once: the hook hands out a stable function
// so it can be a dependency of memos and effects without churning them.
const TRANSLATORS: Record<Lang, Translate> = {
  it: translator("it"),
  en: translator("en"),
};

const LangContext = createContext<Lang>("en");
export const LangProvider = LangContext.Provider;

export function useLang(): Lang {
  return useContext(LangContext);
}

export function useT(): Translate {
  return TRANSLATORS[useContext(LangContext)];
}
