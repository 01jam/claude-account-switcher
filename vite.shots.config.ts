// Renders the real UI in a plain browser, against fixed data, so the website's
// screenshots come from the components rather than from a drawing of them.
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import yaml from "@rollup/plugin-yaml";
import { resolve } from "node:path";

export default defineConfig({
  root: ".shots",
  plugins: [react(), yaml()],
  server: { port: 1421, strictPort: true },
  resolve: {
    alias: {
      "@tauri-apps/api/core": resolve(__dirname, ".shots/mock-core.ts"),
      "@tauri-apps/api/event": resolve(__dirname, ".shots/mock-event.ts"),
      "@tauri-apps/api/window": resolve(__dirname, ".shots/mock-window.ts"),
    },
  },
});
