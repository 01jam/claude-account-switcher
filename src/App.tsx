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
} from "./api";
import AccountList from "./components/AccountList";
import SettingsDialog from "./components/SettingsDialog";
import WindowControls from "./components/WindowControls";
import { useWindowDrag } from "./useWindowDrag";
import {
  LangProvider,
  resolveLang,
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

    const onFocus = () => {
      reload();
      loadUsage();
    };
    window.addEventListener("focus", onFocus);

    // Matched to the backend cache: polling faster only earns 429s.
    const timer = setInterval(() => loadUsage(), 300_000);

    return () => {
      unlistenChanged.then((f) => f());
      unlistenSwitched.then((f) => f());
      unlistenExhausted.then((f) => f());
      window.removeEventListener("focus", onFocus);
      clearInterval(timer);
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
            {t("login.refresh_token")}
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
