import { useEffect } from "react";
import { Button } from "react-aria-components";
import { IconAlertTriangle, IconX } from "@tabler/icons-react";

import type { Notice, NoticeLevel } from "../api";
import { useT } from "../i18n";

/** A notice on screen. The id is the queue's, not the message's: the same
 *  sentence can arrive twice and both times are real. */
export type Toast = Notice & { id: number };

/** How long each level stays up. An error gets longer because it is the one
 *  worth reading twice — and, unlike the rest, it usually leaves a mark
 *  somewhere that outlives the toast. */
const LIFETIME: Record<NoticeLevel, number> = {
  info: 6_000,
  error: 12_000,
};

type Props = {
  toasts: Toast[];
  onDismiss: (id: number) => void;
};

/** Over the list rather than in the flow: most of what these say happened while
 *  the window was elsewhere, and a strip that pushes the accounts down every
 *  time the app has something to report moves the cards out from under the
 *  pointer. */
export default function Toasts({ toasts, onDismiss }: Props) {
  if (toasts.length === 0) return null;

  return (
    <div className="toasts">
      {toasts.map((toast) => (
        <Item key={toast.id} toast={toast} onDismiss={onDismiss} />
      ))}
    </div>
  );
}

function Item({ toast, onDismiss }: { toast: Toast; onDismiss: Props["onDismiss"] }) {
  const t = useT();
  const { id, level } = toast;

  useEffect(() => {
    const timer = setTimeout(() => onDismiss(id), LIFETIME[level]);
    return () => clearTimeout(timer);
  }, [id, level, onDismiss]);

  return (
    <div
      className={`toast${level === "error" ? " toast-error" : ""}`}
      // Errors interrupt a screen reader, the rest wait their turn — the same
      // split the banners they replace had.
      role={level === "error" ? "alert" : "status"}
    >
      {level === "error" && <IconAlertTriangle size={16} className="toast-icon" />}
      <span className="toast-text">{toast.text}</span>
      <Button
        className="toast-close"
        aria-label={t("app.dismiss")}
        onPress={() => onDismiss(id)}
      >
        <IconX size={14} />
      </Button>
    </div>
  );
}
