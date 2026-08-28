import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import yaml from "@rollup/plugin-yaml";

export default defineConfig({
  // The locale files live outside `src` so the Rust side can embed the very
  // same ones; the plugin turns them into plain objects at build time.
  plugins: [react(), yaml()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
});
