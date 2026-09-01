import React from "react";
import ReactDOM from "react-dom/client";
import App from "../src/App";
import "../src/styles.css";

const params = new URLSearchParams(location.search);
const scene = params.get("scene") ?? "window";

// The stylesheet carries its dark palette behind a media query, and headless
// Chrome decides for itself which way that query answers. So the query is
// rewritten to match, or never to — the rules that apply are exactly the ones
// the app ships, and which set applies stops being the browser's call.
function forceTheme(dark: boolean) {
  document.documentElement.style.colorScheme = dark ? "dark" : "light";
  for (const sheet of Array.from(document.styleSheets)) {
    let rules: CSSRuleList;
    try {
      rules = sheet.cssRules;
    } catch {
      continue;
    }
    for (const rule of Array.from(rules)) {
      if (
        rule instanceof CSSMediaRule &&
        rule.conditionText.includes("prefers-color-scheme: dark")
      ) {
        rule.media.mediaText = dark ? "all" : "not all";
      }
    }
  }
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

// The window fills the viewport, so the screenshot is the window.
function press(label: RegExp) {
  const button = Array.from(document.querySelectorAll("button")).find((b) =>
    label.test(b.getAttribute("aria-label") ?? b.textContent ?? ""),
  );
  button?.dispatchEvent(
    new PointerEvent("pointerdown", { bubbles: true, button: 0 }),
  );
  button?.dispatchEvent(
    new PointerEvent("pointerup", { bubbles: true, button: 0 }),
  );
  button?.click();
}

setTimeout(() => {
  forceTheme(params.get("theme") === "dark");
  if (scene === "settings") press(/^Settings/);
  if (scene === "add") press(/^Add account$/);
  // One card on its own: lifted out of the list rather than drawn again, so it
  // is the same element the app renders, at the width the page shows it.
  if (scene === "card") {
    const card = document.querySelector(".account");
    const wrap = document.createElement("div");
    wrap.style.cssText =
      "width:490px;padding:12px;background:var(--bg);font-family:inherit";
    if (card) wrap.appendChild(card);
    document.body.replaceChildren(wrap);
    document.body.style.background = "var(--bg)";
  }
  // Nothing may be focused: a focus ring in a screenshot reads as a state the
  // reader is meant to notice.
  setTimeout(() => {
    (document.activeElement as HTMLElement | null)?.blur();
    document.body.dataset.ready = "1";
  }, 400);
}, 500);
