import { invoke } from "@tauri-apps/api/core";

import type { Lang } from "./i18n";

export type Profile = {
  id: string;
  label: string;
  email: string | null;
  subscription: string | null;
  createdAt: number;
  lastUsed: number | null;
  expiresAt: number | null;
  fiveHourThreshold: number;
  sevenDayThreshold: number;
  active: boolean;
};

export type CurrentAccount = {
  loggedIn: boolean;
  email: string | null;
  subscription: string | null;
  expiresAt: number | null;
  profileId: string | null;
  unsaved: boolean;
};

/** One rolling limit window: how much is spent, and when it resets. */
export type UsageWindow = {
  utilization: number;
  resetsAt: string | null;
};

export type Usage = {
  fiveHour: UsageWindow | null;
  sevenDay: UsageWindow | null;
  fetchedAt: number;
};

export type ProfileUsage = {
  id: string;
  usage: Usage | null;
  error: string | null;
  /** Epoch ms of the next allowed request, while rate-limited. */
  retryAt: number | null;
  /** These numbers are past their TTL and were kept because the endpoint could
   *  not be asked again — the card has to say so rather than look fresh. */
  stale: boolean;
};

/** What pressing Refresh came to: the renewals, and whether the numbers can be
 *  asked for again right now. */
export type RefreshReport = {
  tokens: TokenOutcome[];
  /** A rate-limit cooldown was lifted for this attempt. */
  retried: boolean;
  /** When the endpoint may be asked again, if one is still standing. */
  retryAt: number | null;
};

/** What one account's token renewal came to. */
export type TokenOutcome = {
  id: string;
  label: string;
  status: "renewed" | "fresh" | "deferred" | "failed";
  error: string | null;
};

/** A release newer than the running app, already narrowed to the one file this
 *  machine can use. */
export type UpdateAvailable = {
  version: string;
  notesUrl: string;
  /** `null` when the release carries nothing in this install's format: there is
   *  an update, but fetching it is a manual job. */
  assetName: string | null;
  assetUrl: string | null;
};

export type UpdateStatus = {
  /** The version running right now, straight from the binary. */
  current: string;
  available: UpdateAvailable | null;
};

export type AutoSwitched = {
  from: string;
  to: string;
  reason: string;
};

export type Settings = {
  autoSwitch: boolean;
  startHidden: boolean;
  autostart: boolean;
  /** The saved override; `null` means "follow the system". */
  language: Lang | null;
  /** What "follow the system" resolves to right now. */
  systemLanguage: Lang;
  /** The language in use, override or not. */
  resolvedLanguage: Lang;
};

/** The settings that are plain on/off switches. */
export type ToggleKey = "autoSwitch" | "startHidden" | "autostart";

export const api = {
  listProfiles: () => invoke<Profile[]>("list_profiles"),
  currentAccount: () => invoke<CurrentAccount>("current_account"),
  switchProfile: (id: string) => invoke<void>("switch_profile", { id }),
  saveCurrentAccount: (label?: string) =>
    invoke<Profile>("save_current_account", { label: label ?? null }),
  renameProfile: (id: string, label: string) =>
    invoke<void>("rename_profile", { id, label }),
  deleteProfile: (id: string) => invoke<void>("delete_profile", { id }),
  syncActiveProfile: () => invoke<void>("sync_active_profile"),
  logout: () => invoke<void>("logout"),
  openLoginTerminal: () => invoke<void>("open_login_terminal"),

  fetchUsage: (force = false) =>
    invoke<ProfileUsage[]>("fetch_usage", { force }),
  /** Renew every token that is due and clear the way for a fresh reading. */
  refreshTokens: () => invoke<RefreshReport>("refresh_tokens"),
  /** Renew one account now, whatever its expiry says. */
  refreshProfileToken: (id: string) =>
    invoke<void>("refresh_profile_token", { id }),
  setThresholds: (id: string, fiveHour: number, sevenDay: number) =>
    invoke<void>("set_thresholds", { id, fiveHour, sevenDay }),
  reorderProfiles: (ids: string[]) =>
    invoke<void>("reorder_profiles", { ids }),
  settings: () => invoke<Settings>("get_settings"),
  setAutoSwitch: (enabled: boolean) =>
    invoke<void>("set_auto_switch", { enabled }),
  setStartHidden: (enabled: boolean) =>
    invoke<void>("set_start_hidden", { enabled }),
  setAutostart: (enabled: boolean) =>
    invoke<void>("set_autostart", { enabled }),
  /** `null` hands the choice back to the system locale. */
  setLanguage: (tag: Lang | null) => invoke<void>("set_language", { tag }),

  updateStatus: () => invoke<UpdateStatus>("update_status"),
  /** Downloads the package and hands it to the system installer. Resolves with
   *  the file name the user is about to be shown. */
  installUpdate: () => invoke<string>("install_update"),
  openReleaseNotes: () => invoke<void>("open_release_notes"),
};
