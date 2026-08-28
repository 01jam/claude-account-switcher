import { getCurrentWindow } from "@tauri-apps/api/window";
import type { PointerEvent as ReactPointerEvent } from "react";

/**
 * The corner you pull to resize the window.
 *
 * Without system decorations there are no edges to grab and nothing to say the
 * window resizes at all. This draws the corner and hands the pointer to the
 * window manager, which owns the resize from then on — the same division of
 * labour as `useWindowDrag`.
 *
 * Pointer-only, so it stays out of the tab order: resizing by keyboard is the
 * window manager's own job, and a focusable div here would only be a stop that
 * does nothing when you reach it.
 */
export default function ResizeGrip() {
  const onPointerDown = (event: ReactPointerEvent) => {
    if (event.button !== 0) return;
    // Otherwise the press also begins a text selection that outlives the drag.
    event.preventDefault();
    getCurrentWindow()
      .startResizeDragging("SouthEast")
      .catch((e) => console.warn("window resize failed", e));
  };

  return (
    <div
      className="resize-grip"
      aria-hidden="true"
      onPointerDown={onPointerDown}
    />
  );
}
