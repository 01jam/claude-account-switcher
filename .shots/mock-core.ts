// Stands in for `@tauri-apps/api/core` so the real components can be rendered
// in a browser, with fixed data, for the website's screenshots.
import type {
  CurrentAccount,
  Notice,
  Profile,
  ProfileUsage,
  Settings,
  UpdateStatus,
} from "../src/api";

const MIN = 60_000;
const now = Date.now();

const iso = (ms: number) => new Date(now + ms).toISOString();

const profiles: Profile[] = [
  {
    id: "work",
    label: "Work",
    email: "work@01jam.dev",
    subscription: "Max",
    createdAt: now,
    lastUsed: now - 2 * 3600_000,
    expiresAt: now + 4 * 3600_000,
    fiveHourThreshold: 90,
    sevenDayThreshold: 85,
    active: true,
  },
  {
    id: "personal",
    label: "Personal",
    email: "me@01jam.dev",
    subscription: "Pro",
    createdAt: now,
    lastUsed: now - 30 * 3600_000,
    expiresAt: now + 3 * 3600_000,
    fiveHourThreshold: 90,
    sevenDayThreshold: 90,
    active: false,
  },
  {
    id: "client",
    label: "Client",
    email: "dev@example.com",
    subscription: "Pro",
    createdAt: now,
    lastUsed: now - 6 * 86_400_000,
    expiresAt: now + 5 * 3600_000,
    fiveHourThreshold: 100,
    sevenDayThreshold: 100,
    active: false,
  },
];

const usage: ProfileUsage[] = [
  {
    id: "work",
    usage: {
      fiveHour: { utilization: 72, resetsAt: iso(135 * MIN) },
      sevenDay: { utilization: 38, resetsAt: iso((3 * 24 + 4) * 60 * MIN) },
      fetchedAt: now - 40_000,
    },
    error: null,
    retryAt: null,
    stale: false,
  },
  {
    id: "personal",
    usage: {
      fiveHour: { utilization: 91, resetsAt: iso(47 * MIN) },
      sevenDay: { utilization: 64, resetsAt: iso((1 * 24 + 9) * 60 * MIN) },
      fetchedAt: now - 90_000,
    },
    error: null,
    retryAt: null,
    stale: false,
  },
  {
    id: "client",
    usage: {
      fiveHour: { utilization: 12, resetsAt: iso(212 * MIN) },
      sevenDay: { utilization: 47, resetsAt: iso((5 * 24 + 2) * 60 * MIN) },
      fetchedAt: now - 3 * 3600_000,
    },
    error: null,
    retryAt: null,
    stale: true,
  },
];

const current: CurrentAccount = {
  loggedIn: true,
  email: "work@01jam.dev",
  subscription: "Max",
  expiresAt: now + 4 * 3600_000,
  profileId: "work",
  unsaved: false,
};

const settings: Settings = {
  autoSwitch: true,
  startHidden: false,
  startOnFreest: true,
  autostart: true,
  language: null,
  systemLanguage: "en",
  resolvedLanguage: "en",
};

const params = new URLSearchParams(location.search);

const update: UpdateStatus = params.get("update") === "0"
  ? { current: "0.2.7", available: null }
  : {
      current: "0.2.7",
      available: {
        version: "0.2.8",
        notesUrl: "https://github.com/01jam/claude-account-switcher/releases/tag/v0.2.8",
        assetName: "claude-account-switcher_0.2.8_amd64.deb",
        assetUrl: "https://example.invalid",
        assetSize: 3_175_268,
        installs: true,
      },
    };

// The two states that only exist after something happened in the background,
// reachable here so they can be shot like any other: `?notices=1` for the
// toasts, `?token-error=1` for the mark on a card.
const notices: Notice[] =
  params.get("notices") === "1"
    ? [
        {
          text: "Switched to \u201cPersonal\u201d: Work reached 5-hour session at 91% (threshold 90%).",
          level: "info",
        },
        {
          text: "Cannot renew \u201cWork\u201d: the refresh token was refused \u2014 sign in again with `claude`",
          level: "error",
        },
      ]
    : [];

const answers: Record<string, unknown> = {
  list_profiles: profiles,
  current_account: current,
  get_settings: settings,
  fetch_usage: usage,
  update_status: update,
  startup_pick: null,
  pending_notices: notices,
  token_errors:
    params.get("token-error") === "1"
      ? { work: "the refresh token was refused — sign in again with `claude`" }
      : {},
};

export function invoke<T>(cmd: string): Promise<T> {
  return Promise.resolve((answers[cmd] ?? null) as T);
}
