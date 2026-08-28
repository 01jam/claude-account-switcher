import { getCurrentWindow } from "@tauri-apps/api/window";
import { Button } from "react-aria-components";
import { IconMinus, IconX } from "@tabler/icons-react";

import { useT } from "../i18n";

/**
 * Minimise and close, drawn by us.
 *
 * The window carries no system decorations, so without these there would be no
 * way to put it away. There is deliberately no maximise: a list of a handful of
 * accounts has nothing to do with a full screen.
 *
 * Close goes through `close()` rather than `hide()` so it takes the same path
 * as every other close — the backend answers it by hiding to the tray.
 */
export default function WindowControls() {
  const t = useT();

  return (
    <div className="win-controls">
      <Button
        className="win-control"
        aria-label={t("window.minimize")}
        onPress={() => void getCurrentWindow().minimize()}
      >
        <IconMinus size={15} stroke={1.8} />
      </Button>
      <Button
        className="win-control win-control-close"
        aria-label={t("window.close")}
        onPress={() => void getCurrentWindow().close()}
      >
        <IconX size={15} stroke={1.8} />
      </Button>
    </div>
  );
}
