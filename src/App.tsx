import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  Button,
  Dialog,
  DialogTrigger,
  Heading,
  Modal,
  ModalOverlay,
} from "react-aria-components";
import {
  IconAlertTriangle,
  IconArrowsExchange,
  IconDeviceFloppy,
  IconLogout,
  IconRefresh,
  IconSettings,
  IconTerminal2,
  IconUserPlus,
} from "@tabler/icons-react";

import {
  api,
  type AutoSwitched,
  type CurrentAccount,
  type Profile,
  type ProfileUsage,
  type Settings,
  type ToggleKey,
  type TokenOutcome,
  type RefreshReport,
} from "./api";
import AccountList from "./components/AccountList";
import ResizeGrip from "./components/ResizeGrip";
import SettingsDialog from "./components/SettingsDialog";
import WindowControls from "./components/WindowControls";
import { useWindowDrag } from "./useWindowDrag";
import {
  LangProvider,
  resolveLang,
  useLang,
  useT,
  type Lang,
  type Translate,
} from "./i18n";

export default function App() {
  // The backend is the authority on the language, but it takes a round-trip to
  // answer; starting from the browser's own locale keeps the first paint from
  // being in the wrong one.
  const [lang, setLang] = useState<Lang>(() => resolveLang(navigator.language));

  return (
    <LangProvider value={lang}>
      <Switcher onLanguage={setLang} />
    </LangProvider>
  );
}

function Switcher({ onLanguage }: { onLanguage: (lang: Lang) => void }) {
  const t = useT();
  const lang = useLang();
  const drag = useWindowDrag();
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [current, setCurrent] = useState<CurrentAccount | null>(null);
  const [usage, setUsage] = useState<Record<string, ProfileUsage>>({});
  const [settings, setSettings] = useState<Settings | null>(null);
  const [showSettings, setShowSettings] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const reload = useCallback(async () => {
    try {
      const [list, account, config] = await Promise.all([
        api.listProfiles(),
        api.currentAccount(),
        api.settings(),
      ]);
      setProfiles(list);
      setCurrent(account);
      setSettings(config);
      onLanguage(config.resolvedLanguage);
    } catch (e) {
      setError(String(e));
    }
  }, [onLanguage]);

  // Usage is a network round-trip per account, so it loads on its own and never
  // blocks the list from rendering.
  const loadUsage = useCallback(async (force = false) => {
    try {
      const entries = await api.fetchUsage(force);
      setUsage(Object.fromEntries(entries.map((e) => [e.id, e])));
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    reload();
    loadUsage();
    const unlistenChanged = listen("profiles-changed", reload);
    const unlistenSwitched = listen<AutoSwitched>("auto-switched", (e) => {
      setNotice(t("autoswitch.switched", e.payload));
      reload();
      loadUsage(true);
    });
    const unlistenExhausted = listen<string>("auto-switch-exhausted", (e) => {
      setNotice(t("autoswitch.exhausted", { reason: e.payload }));
    });
    const unlistenToken = listen<TokenOutcome>("token-refresh-failed", (e) => {
      setError(
        t("tokens.failed", {
          name: e.payload.label,
          error: e.payload.error ?? "",
        }),
      );
    });

    // The meters are pushed, not pulled. A timer here would be the webview's,
    // and the webview is asleep for most of this app's life — it lives in the
    // tray. The task that sends this runs whether the window is up or not.
    const unlistenUsage = listen<ProfileUsage[]>("usage-updated", (e) => {
      setUsage(Object.fromEntries(e.payload.map((u) => [u.id, u])));
    });

    // Coming back into view: a push that landed while the window was hidden is
    // not replayed, so the cache is re-read on the way in — the poll has been
    // keeping it current the whole time, so this costs no request. `focus`
    // alone misses being shown from the tray, which on Linux does not reliably
    // reach the page as one.
    const onWake = () => {
      if (document.visibilityState !== "visible") return;
      reload();
      loadUsage();
    };
    window.addEventListener("focus", onWake);
    document.addEventListener("visibilitychange", onWake);

    return () => {
      unlistenChanged.then((f) => f());
      unlistenSwitched.then((f) => f());
      unlistenExhausted.then((f) => f());
      unlistenToken.then((f) => f());
      unlistenUsage.then((f) => f());
      window.removeEventListener("focus", onWake);
      document.removeEventListener("visibilitychange", onWake);
    };
  }, [reload, loadUsage, t]);

  const run = useCallback(
    async (fn: () => Promise<unknown>) => {
      setBusy(true);
      setError(null);
      try {
        await fn();
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
        await reload();
      }
    },
    [reload],
  );

  return (
    <main className="app">
      {/* The window has no system decorations: this bar is what moves it, and
          what puts it away. Settings rides here rather than in the header —
          it is chrome, not one of the list's own actions. */}
      <div className="titlebar" {...drag}>
        <Button
          className="win-control"
          aria-label={t("app.settings")}
          onPress={() => setShowSettings(true)}
        >
          <IconSettings size={16} />
        </Button>
        <WindowControls />
      </div>

      <ResizeGrip />

      <header className="app-header">
        <div>
          <h1>{t("app.title")}</h1>
          <p className="subtitle">{statusLine(t, current, profiles)}</p>
        </div>
        <div className="app-header-actions">
          <Button
            className="btn btn-ghost btn-icon"
            aria-label={t("app.refresh")}
            isDisabled={busy}
            onPress={() =>
              run(async () => {
                await api.syncActiveProfile();
                // Renewing first is what makes the numbers reachable at all for
                // an account whose token has expired — and pressing this button
                // has to visibly do something either way.
                setNotice(refreshNotice(t, lang, await api.refreshTokens()));
                await loadUsage(true);
              })
            }
          >
            <IconRefresh size={18} />
          </Button>
        </div>
      </header>

      <Button
        className={`auto-banner${settings?.autoSwitch ? " auto-banner-on" : ""}`}
        onPress={() => setShowSettings(true)}
      >
        <IconArrowsExchange size={16} />
        <span>
          {t(settings?.autoSwitch ? "autoswitch.banner_on" : "autoswitch.banner_off")}
        </span>
        <IconSettings size={15} className="auto-banner-cog" />
      </Button>

      {error && (
        <div className="banner banner-error" role="alert">
          <IconAlertTriangle size={18} />
          <span>{error}</span>
        </div>
      )}

      {notice && (
        <div className="banner banner-info" role="status">
          <span>{notice}</span>
          <Button className="btn btn-ghost btn-sm" onPress={() => setNotice(null)}>
            {t("app.ok")}
          </Button>
        </div>
      )}

      {current?.unsaved && (
        <div className="banner banner-info">
          <div>
            <strong>{t("login.unsaved_title")}</strong>
            <p>
              {t("login.unsaved_body", {
                name: current.email ?? t("login.unsaved_fallback"),
              })}
            </p>
          </div>
          <Button
            className="btn btn-primary"
            isDisabled={busy}
            onPress={() => run(() => api.saveCurrentAccount())}
          >
            <IconDeviceFloppy size={16} />
            {t("app.save")}
          </Button>
        </div>
      )}

      <div className="list-wrap">
        <AccountList
          profiles={profiles}
          usage={usage}
          busy={busy}
          autoSwitch={settings?.autoSwitch ?? false}
          onSwitch={(id) => run(() => api.switchProfile(id))}
          onRefreshToken={(id) =>
            run(async () => {
              await api.refreshProfileToken(id);
              await loadUsage(true);
            })
          }
          onRename={(id, label) => run(() => api.renameProfile(id, label))}
          onDelete={(id) => run(() => api.deleteProfile(id))}
          onThresholds={(id, fiveHour, sevenDay) =>
            run(() => api.setThresholds(id, fiveHour, sevenDay))
          }
          onReorder={(ids) => run(() => api.reorderProfiles(ids))}
        />
      </div>

      <footer className="app-footer">
        <AddAccountDialog
          busy={busy}
          onOpenTerminal={() => run(api.openLoginTerminal)}
          onLogout={() => run(api.logout)}
          onSave={() => run(() => api.saveCurrentAccount())}
        />
        {current?.loggedIn && !current.unsaved && (
          <Button
            className="btn btn-ghost"
            isDisabled={busy}
            onPress={() => run(() => api.saveCurrentAccount())}
          >
            <IconDeviceFloppy size={16} />
            {t("login.save_current")}
          </Button>
        )}
      </footer>

      <SettingsDialog
        isOpen={showSettings}
        settings={settings}
        busy={busy}
        onClose={() => setShowSettings(false)}
        onChange={(key: ToggleKey, value: boolean) => {
          // Optimistic: the toggle should not lag behind the pointer.
          setSettings((s) => (s ? { ...s, [key]: value } : s));
          const call =
            key === "autoSwitch"
              ? api.setAutoSwitch
              : key === "autostart"
                ? api.setAutostart
                : api.setStartHidden;
          run(() => call(value));
        }}
        onLanguageChange={(tag) => {
          // Same reasoning as the toggles, and here the whole window repaints
          // in the new language the moment the reload comes back.
          setSettings((s) =>
            s ? { ...s, language: tag, resolvedLanguage: tag ?? s.systemLanguage } : s,
          );
          onLanguage(tag ?? settings?.systemLanguage ?? "en");
          run(() => api.setLanguage(tag));
        }}
      />
    </main>
  );
}

/** What a press of Refresh has to say for itself. Reporting "nothing was due"
 *  is the point: a button that answers with silence reads as a broken one. */
function refreshNotice(
  t: Translate,
  lang: Lang,
  report: RefreshReport,
): string {
  const failed = report.tokens.find((o) => o.status === "failed");
  if (failed) {
    return t("tokens.failed", {
      name: failed.label,
      error: failed.error ?? "",
    });
  }
  const renewed = report.tokens.filter((o) => o.status === "renewed");
  if (renewed.length === 1) {
    return t("tokens.renewed_one", { name: renewed[0].label });
  }
  if (renewed.length > 1) {
    return t("tokens.renewed_many", { count: renewed.length });
  }
  // Still standing means this press did not get to override it — the previous
  // one did, moments ago. Saying when beats leaving the numbers unexplained.
  if (report.retryAt) {
    return t("usage.retry_blocked", {
      time: new Date(report.retryAt).toLocaleTimeString(lang, {
        hour: "2-digit",
        minute: "2-digit",
      }),
    });
  }
  return t("tokens.all_fresh");
}

function statusLine(
  t: Translate,
  current: CurrentAccount | null,
  profiles: Profile[],
) {
  if (!current) return t("app.loading");
  if (!current.loggedIn) return t("status.logged_out");
  return t("status.active", {
    name: current.email ?? t("status.unknown_account"),
    plan: current.subscription ? ` · ${current.subscription}` : "",
    count: profiles.length,
  });
}

type AddProps = {
  busy: boolean;
  onOpenTerminal: () => void;
  onLogout: () => void;
  onSave: () => void;
};

// Claude Code owns the OAuth flow, so adding an account is: log out, let the
// CLI log in from a terminal, then capture what it left behind.
function AddAccountDialog({ busy, onOpenTerminal, onLogout, onSave }: AddProps) {
  const t = useT();

  return (
    <DialogTrigger>
      <Button className="btn btn-primary" isDisabled={busy}>
        <IconUserPlus size={16} />
        {t("login.add")}
      </Button>
      <ModalOverlay className="overlay" isDismissable>
        <Modal className="modal">
          <Dialog className="dialog">
            {({ close }) => (
              <>
                <Heading slot="title">{t("login.add_title")}</Heading>
                <ol className="steps">
                  <li>
                    {t("login.step_logout")}
                    <Button
                      className="btn btn-ghost btn-sm"
                      onPress={onLogout}
                      isDisabled={busy}
                    >
                      <IconLogout size={15} />
                      {t("login.logout")}
                    </Button>
                  </li>
                  <li>
                    {t("login.step_terminal")}
                    <Button
                      className="btn btn-ghost btn-sm"
                      onPress={onOpenTerminal}
                      isDisabled={busy}
                    >
                      <IconTerminal2 size={15} />
                      {t("login.open_terminal")}
                    </Button>
                  </li>
                  <li>
                    {t("login.step_save")}
                    <Button
                      className="btn btn-ghost btn-sm"
                      onPress={() => {
                        onSave();
                        close();
                      }}
                      isDisabled={busy}
                    >
                      <IconDeviceFloppy size={15} />
                      {t("login.save_account")}
                    </Button>
                  </li>
                </ol>
                <div className="dialog-actions">
                  <Button className="btn btn-ghost" onPress={close}>
                    {t("app.close")}
                  </Button>
                </div>
              </>
            )}
          </Dialog>
        </Modal>
      </ModalOverlay>
    </DialogTrigger>
  );
}
