import { defineConfig } from "vitest/config";

export default defineConfig({
  clearScreen: false,
  // Tauri loads the dev server from a fixed port. Bind to loopback only so it
  // is never reachable from the LAN.
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "es2022",
    // One bundle on purpose: every language file is compiled in (about
    // 50 kB each), and the window loads it from disk, not a network.
    chunkSizeWarningLimit: 2048,
  },
  test: {
    include: ["src/**/*.test.ts"],
    // The tests read the window's words in Traditional Chinese, the
    // language they were written in; English is only the fallback.
    setupFiles: ["src/test-setup.ts"],
    // No transformed modules kept across runs: the language loader's
    // `import.meta.glob` would keep the files it found when it was cached,
    // and a language file added since would not be seen.
    fsModuleCache: false,
    environment: "node",
  },
});
