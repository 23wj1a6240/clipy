import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
// Brand font — Gabarito carries display + UI everywhere (design-system §4).
// Bundled locally; a desktop app must not fetch fonts from a CDN.
import "@fontsource-variable/gabarito";
import "./styles.css";
import { applyPlatformAttribute } from "./lib/platform";
import { DesktopIntl } from "./lib/i18n";

// Stamp `data-os` before the first render so platform-scoped CSS applies to
// the initial paint rather than swapping in a frame later.
applyPlatformAttribute();

// Suppress the webview's page context menu.
//
// WebView2 offers "Print", "Save As" and "Share" on right-click, which is a
// browser's menu leaking through a native app: Save As offers to write the
// UI's HTML to disk, and Share opens an empty Windows share sheet because
// there is no shareable document behind it. macOS WKWebView shows its own
// variant. Neither belongs in a recorder.
//
// Text fields keep their menu — that one is genuinely useful (cut/copy/paste,
// spelling) and is the reason this is not a blanket suppression.
document.addEventListener("contextmenu", (event) => {
  const target = event.target as HTMLElement | null;
  const editable = target?.closest?.(
    "input, textarea, [contenteditable=''], [contenteditable='true']",
  );
  if (!editable) event.preventDefault();
});

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <DesktopIntl>
      <App />
    </DesktopIntl>
  </React.StrictMode>
);
