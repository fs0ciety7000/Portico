import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import pkg from "./package.json" with { type: "json" };

// Tauri expects a fixed port and no screen clearing (keeps Rust errors visible).
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  // Shown in the status bar; package.json follows the workspace version.
  define: { __APP_VERSION__: JSON.stringify(pkg.version) },
  server: { port: 1420, strictPort: true, watch: { ignored: ["**/src-tauri/**"] } },
  build: { target: "es2022", sourcemap: false },
});
