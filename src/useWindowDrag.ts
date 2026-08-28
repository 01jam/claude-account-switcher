import { getCurrentWindow } from "@tauri-apps/api/window";
import type { PointerEvent as ReactPointerEvent } from "react";

/**
 * Drag the window by its bar, without eating the clicks that live on it.
 *
 * Tauri's own `data-tauri-drag-region` starts the drag on `mousedown`. From
 * that moment the window manager owns the pointer and the `click` that would
 * have followed never arrives — even when the mouse never moved. Buttons
 * sitting on the bar stop working.
 *
 * So the drag starts on *movement* instead: a press that stays put is still a
 * click and reaches its target, a press that travels a few pixels becomes a
 * drag. One handler on the bar rather than an attribute per element — a button
 * is recognisably a button, and adding a new container cannot silently break
 * dragging.
 */

/** How far the pointer may travel and still count as a click. */
const CLICK_SLOP = 4;

/* Anything that answers clicks does not drag. `[data-no-drag]` covers what is
   not an interactive element but behaves like one. */
const INTERACTIVE =
  "button, a, input, select, textarea, [role='button'], [data-no-drag]";

export function useWindowDrag() {
  return {
    onPointerDown(event: ReactPointerEvent) {
      if (event.button !== 0) return;
      if ((event.target as HTMLElement | null)?.closest(INTERACTIVE)) return;

      const start = { x: event.clientX, y: event.clientY };

      const onMove = (move: PointerEvent) => {
        const travelled =
          Math.abs(move.clientX - start.x) + Math.abs(move.clientY - start.y);
        if (travelled < CLICK_SLOP) return;
        detach();
        getCurrentWindow()
          .startDragging()
          .catch((e) => console.warn("window drag failed", e));
      };

      const detach = () => {
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", detach);
        window.removeEventListener("pointercancel", detach);
      };

      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", detach);
      window.addEventListener("pointercancel", detach);
    },
  };
}
