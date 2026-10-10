import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
// @ts-expect-error the import fails type-checking without @types/node
import process from "node:process";
const host = process.env.TAURI_DEV_HOST;

export default defineConfig(() => ({
  plugins: [vue()],
  // Vite clearing the terminal would hide cargo errors.
  clearScreen: false,
  // Tauri expects a fixed port. Fail if the port is unavailable.
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // Cargo rebuilds rewrite src-tauri; watching it would trigger reloads.
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    rollupOptions: {
      input: {
        index: "index.html",
        settings: "settings.html",
      },
    },
  },
}));
