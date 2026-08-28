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
};
