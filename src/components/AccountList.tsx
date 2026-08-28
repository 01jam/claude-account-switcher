import { useState } from "react";
import {
  Button,
  Dialog,
  GridList,
  GridListItem,
  Heading,
  Input,
  Label,
  Menu,
  MenuItem,
  MenuTrigger,
  Modal,
  ModalOverlay,
  Popover,
  TextField,
  useDragAndDrop,
} from "react-aria-components";
import {
  IconAlertTriangle,
  IconArrowsExchange,
  IconCircleCheckFilled,
  IconDotsVertical,
  IconGripVertical,
  IconPencil,
  IconRefreshAlert,
  IconTrash,
  IconUserCircle,
  IconUsersGroup,
} from "@tabler/icons-react";

import type { Profile, ProfileUsage } from "../api";
import { useLang, useT, type Lang, type Translate } from "../i18n";
import UsageMeter from "./UsageMeter";

type Props = {
  profiles: Profile[];
  usage: Record<string, ProfileUsage>;
  busy: boolean;
  /** Off means the thresholds are set but nothing acts on them. */
  autoSwitch: boolean;
  onSwitch: (id: string) => void;
  /** Renew this account's OAuth token now, whatever its expiry says. */
  onRefreshToken: (id: string) => void;
  onRename: (id: string, label: string) => void;
  onDelete: (id: string) => void;
  onThresholds: (id: string, fiveHour: number, sevenDay: number) => void;
  onReorder: (ids: string[]) => void;
};

export default function AccountList({
  profiles,
  usage,
  busy,
  autoSwitch,
  onSwitch,
  onRefreshToken,
  onRename,
  onDelete,
  onThresholds,
  onReorder,
}: Props) {
  const t = useT();
  const lang = useLang();
  const [editing, setEditing] = useState<Profile | null>(null);
  const [confirming, setConfirming] = useState<Profile | null>(null);
  // Thresholds shown while dragging, before the value is persisted.
  const [preview, setPreview] = useState<Record<string, [number, number]>>({});

  const { dragAndDropHooks } = useDragAndDrop({
    getItems: (keys) =>
      [...keys].map((key) => ({ "text/plain": String(key) })),
    onReorder(e) {
      const ids = profiles.map((p) => p.id);
      const moving = [...e.keys].map(String);
      const rest = ids.filter((id) => !moving.includes(id));
      const at = rest.indexOf(String(e.target.key));
      if (at < 0) return;
      const cut = e.target.dropPosition === "before" ? at : at + 1;
      onReorder([...rest.slice(0, cut), ...moving, ...rest.slice(cut)]);
    },
  });

  if (profiles.length === 0) {
    return (
      <div className="empty">
        <IconUsersGroup size={32} stroke={1.5} />
        <p>{t("accounts.empty_title")}</p>
        <p className="muted">{t("accounts.empty_hint")}</p>
      </div>
    );
  }

  return (
    <>
      <GridList
        className="accounts"
        aria-label={t("accounts.list_label")}
        selectionMode="none"
        items={profiles}
        dragAndDropHooks={dragAndDropHooks}
        disabledKeys={busy ? profiles.map((p) => p.id) : []}
      >
        {(profile) => {
          const entry = usage[profile.id];
          const [fh, sd] = preview[profile.id] ?? [
            profile.fiveHourThreshold,
            profile.sevenDayThreshold,
          ];
          const setPreviewFor = (five: number, seven: number) =>
            setPreview((p) => ({ ...p, [profile.id]: [five, seven] }));

          return (
            <GridListItem
              className={`account${profile.active ? " account-active" : ""}`}
              id={profile.id}
              textValue={profile.label}
            >
              <div className="account-row">
                <Button
                  slot="drag"
                  className="account-grip"
                  aria-label={t("accounts.reorder", { name: profile.label })}
                >
                  <IconGripVertical size={16} />
                </Button>

                <span className="account-avatar" aria-hidden>
                  {profile.active ? (
                    <IconCircleCheckFilled size={22} />
                  ) : (
                    <IconUserCircle size={22} stroke={1.5} />
                  )}
                </span>

                <span className="account-body">
                  <span className="account-name">{profile.label}</span>
                  <span className="account-meta">
                    {[
                      profile.email,
                      profile.subscription,
                      lastUsed(t, profile),
                      expired(profile) ? t("accounts.token_expired") : null,
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </span>
                </span>

                <MenuTrigger>
                  <Button
                    className="btn btn-ghost btn-icon"
                    aria-label={t("accounts.actions_for", { name: profile.label })}
                  >
                    <IconDotsVertical size={18} />
                  </Button>
                  <Popover className="popover">
                    <Menu
                      className="menu"
                      onAction={(key) => {
                        if (key === "rename") setEditing(profile);
                        if (key === "refresh") onRefreshToken(profile.id);
                        if (key === "delete") setConfirming(profile);
                      }}
                    >
                      <MenuItem className="menu-item" id="rename">
                        <IconPencil size={16} />
                        {t("accounts.rename")}
                      </MenuItem>
                      <MenuItem className="menu-item" id="refresh">
                        <IconRefreshAlert size={16} />
                        {t("accounts.refresh_token")}
                      </MenuItem>
                      <MenuItem
                        className="menu-item menu-item-danger"
                        id="delete"
                      >
                        <IconTrash size={16} />
                        {t("app.delete")}
                      </MenuItem>
                    </Menu>
                  </Popover>
                </MenuTrigger>
              </div>

              <div className="account-meters">
                <UsageMeter
                  label={t("usage.five_hour")}
                  window={entry?.usage?.fiveHour ?? null}
                  threshold={fh}
                  isDisabled={busy}
                  isArmed={autoSwitch}
                  onPreview={(v) => setPreviewFor(v, sd)}
                  onCommit={(v) => onThresholds(profile.id, v, sd)}
                />
                <UsageMeter
                  label={t("usage.seven_day")}
                  window={entry?.usage?.sevenDay ?? null}
                  threshold={sd}
                  isDisabled={busy}
                  isArmed={autoSwitch}
                  onPreview={(v) => setPreviewFor(fh, v)}
                  onCommit={(v) => onThresholds(profile.id, fh, v)}
                />
              </div>

              {!entry?.error && entry?.stale && (
                <p className="account-usage-note">
                  {staleLine(t, entry.usage?.fetchedAt)}
                  {retryLine(t, lang, entry.retryAt)}
                </p>
              )}

              {entry?.error && (
                <p className="account-usage-error">
                  <IconAlertTriangle size={14} />
                  <span>
                    {entry.error}
                    {retryLine(t, lang, entry.retryAt)}
                  </span>
                </p>
              )}

              {profile.active ? (
                // Static rather than a disabled button: it states a fact, and
                // an unreachable focus stop on every card would be noise.
                <p className="account-current">
                  <IconCircleCheckFilled size={16} />
                  {t("accounts.in_use")}
                </p>
              ) : (
                <Button
                  className="btn btn-primary account-use"
                  isDisabled={busy}
                  onPress={() => onSwitch(profile.id)}
                >
                  <IconArrowsExchange size={16} />
                  {t("accounts.switch_to")}
                </Button>
              )}
            </GridListItem>
          );
        }}
      </GridList>

      <RenameDialog
        profile={editing}
        onClose={() => setEditing(null)}
        onSubmit={(label) => {
          if (editing) onRename(editing.id, label);
          setEditing(null);
        }}
      />

      <ModalOverlay
        className="overlay"
        isOpen={confirming !== null}
        onOpenChange={(open) => !open && setConfirming(null)}
        isDismissable
      >
        <Modal className="modal">
          <Dialog className="dialog" role="alertdialog">
            <Heading slot="title">
              {t("accounts.delete_title", { name: confirming?.label ?? "" })}
            </Heading>
            <p className="muted">{t("accounts.delete_body")}</p>
            <div className="dialog-actions">
              <Button
                className="btn btn-ghost"
                onPress={() => setConfirming(null)}
              >
                {t("app.cancel")}
              </Button>
              <Button
                className="btn btn-danger"
                onPress={() => {
                  if (confirming) onDelete(confirming.id);
                  setConfirming(null);
                }}
              >
                {t("app.delete")}
              </Button>
            </div>
          </Dialog>
        </Modal>
      </ModalOverlay>
    </>
  );
}

function RenameDialog({
  profile,
  onClose,
  onSubmit,
}: {
  profile: Profile | null;
  onClose: () => void;
  onSubmit: (label: string) => void;
}) {
  const t = useT();
  const [value, setValue] = useState("");

  return (
    <ModalOverlay
      className="overlay"
      isOpen={profile !== null}
      onOpenChange={(open) => {
        if (open) setValue(profile?.label ?? "");
        else onClose();
      }}
      isDismissable
    >
      <Modal className="modal">
        <Dialog className="dialog">
          <Heading slot="title">{t("accounts.rename_title")}</Heading>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              if (value.trim()) onSubmit(value.trim());
            }}
          >
            <TextField
              className="field"
              autoFocus
              value={value}
              onChange={setValue}
            >
              <Label>{t("accounts.name_field")}</Label>
              <Input className="input" />
            </TextField>
            <div className="dialog-actions">
              <Button className="btn btn-ghost" onPress={onClose}>
                {t("app.cancel")}
              </Button>
              <Button className="btn btn-primary" type="submit">
                {t("app.save")}
              </Button>
            </div>
          </form>
        </Dialog>
      </Modal>
    </ModalOverlay>
  );
}

// How old the numbers on the card are. Only ever shown when they are past their
// TTL: fresh ones need no caption.
function staleLine(t: Translate, fetchedAt: number | undefined) {
  if (!fetchedAt) return null;
  const minutes = Math.round((Date.now() - fetchedAt) / 60_000);
  return minutes < 60
    ? t("usage.stale_minutes", { minutes })
    : t("usage.stale_hours", { hours: Math.round(minutes / 60) });
}

// The API's 429 carries no deadline of its own, so what is shown is when this
// app will try again.
function retryLine(t: Translate, lang: Lang, retryAt: number | null | undefined) {
  if (!retryAt) return null;
  const minutes = Math.ceil((retryAt - Date.now()) / 60_000);
  if (minutes <= 0) return t("usage.retry_soon");
  const time = new Date(retryAt).toLocaleTimeString(lang, {
    hour: "2-digit",
    minute: "2-digit",
  });
  return t("usage.retry_at", { time, minutes });
}

// Expired only means the access token needs a refresh, which Claude Code does
// itself on the next run — worth showing, not worth blocking a switch.
function expired(profile: Profile) {
  return profile.expiresAt !== null && profile.expiresAt < Date.now();
}

function lastUsed(t: Translate, profile: Profile) {
  if (!profile.lastUsed) return null;
  const days = Math.floor((Date.now() - profile.lastUsed) / 86_400_000);
  if (days <= 0) return t("accounts.used_today");
  if (days === 1) return t("accounts.used_yesterday");
  return t("accounts.used_days_ago", { days });
}
