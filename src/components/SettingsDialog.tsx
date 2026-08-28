import {
  Button,
  Dialog,
  Heading,
  Label,
  ListBox,
  ListBoxItem,
  Modal,
  ModalOverlay,
  Popover,
  Select,
  SelectValue,
  Switch,
} from "react-aria-components";
import { IconChevronDown } from "@tabler/icons-react";

import type { Settings, ToggleKey } from "../api";
import { LANGS, LANG_NAMES, useT, type Lang, type Translate } from "../i18n";

type Props = {
  isOpen: boolean;
  settings: Settings | null;
  busy: boolean;
  onClose: () => void;
  onChange: (key: ToggleKey, value: boolean) => void;
  /** `null` gives the choice back to the system locale. */
  onLanguageChange: (tag: Lang | null) => void;
};

const TOGGLES: { key: ToggleKey; title: string; hint: string }[] = [
  {
    key: "autoSwitch",
    title: "settings.auto_switch_title",
    hint: "settings.auto_switch_hint",
  },
  {
    key: "autostart",
    title: "settings.autostart_title",
    hint: "settings.autostart_hint",
  },
  {
    key: "startHidden",
    title: "settings.start_hidden_title",
    hint: "settings.start_hidden_hint",
  },
];

/** The select works in plain strings, so "follow the system" needs a key of its
 *  own — `null` is not something a ListBox can carry. */
const AUTO = "auto";

export default function SettingsDialog({
  isOpen,
  settings,
  busy,
  onClose,
  onChange,
  onLanguageChange,
}: Props) {
  const t = useT();

  return (
    <ModalOverlay
      className="overlay"
      isOpen={isOpen}
      onOpenChange={(open) => !open && onClose()}
      isDismissable
    >
      <Modal className="modal">
        <Dialog className="dialog">
          <Heading slot="title">{t("settings.title")}</Heading>

          <div className="settings">
            {TOGGLES.map((toggle) => (
              <Switch
                key={toggle.key}
                className="setting"
                isSelected={settings?.[toggle.key] ?? false}
                isDisabled={busy || !settings}
                onChange={(value) => onChange(toggle.key, value)}
              >
                <div className="switch-indicator" />
                <div>
                  <strong>{t(toggle.title)}</strong>
                  <p className="muted">{t(toggle.hint)}</p>
                </div>
              </Switch>
            ))}

            <LanguageSetting
              t={t}
              settings={settings}
              busy={busy}
              onLanguageChange={onLanguageChange}
            />
          </div>

          <div className="dialog-actions">
            <Button className="btn btn-primary" onPress={onClose}>
              {t("app.close")}
            </Button>
          </div>
        </Dialog>
      </Modal>
    </ModalOverlay>
  );
}

function LanguageSetting({
  t,
  settings,
  busy,
  onLanguageChange,
}: {
  t: Translate;
  settings: Settings | null;
  busy: boolean;
  onLanguageChange: (tag: Lang | null) => void;
}) {
  // Naming the language the automatic option resolves to is the difference
  // between a setting the user can predict and one they have to try.
  const auto = t("settings.language_auto", {
    name: LANG_NAMES[settings?.systemLanguage ?? "en"],
  });

  return (
    <Select
      className="setting setting-select"
      selectedKey={settings?.language ?? AUTO}
      isDisabled={busy || !settings}
      onSelectionChange={(key) =>
        onLanguageChange(key === AUTO ? null : (key as Lang))
      }
    >
      <Label>{t("settings.language_title")}</Label>
      <Button className="select-trigger">
        <SelectValue />
        <IconChevronDown size={16} />
      </Button>
      <p className="muted">{t("settings.language_hint")}</p>
      <Popover className="popover popover-select">
        <ListBox className="menu">
          <ListBoxItem className="menu-item" id={AUTO} textValue={auto}>
            {auto}
          </ListBoxItem>
          {LANGS.map((lang) => (
            <ListBoxItem
              key={lang}
              className="menu-item"
              id={lang}
              textValue={LANG_NAMES[lang]}
            >
              {LANG_NAMES[lang]}
            </ListBoxItem>
          ))}
        </ListBox>
      </Popover>
    </Select>
  );
}
