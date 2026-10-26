import { defineConfig } from "vite";

// The frontend sources live in src/ (plan §4.1); Tauri expects a fixed dev
// port and no screen clearing.
export default defineConfig({
  root: "src",
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: "es2021",
    outDir: "../dist",
    emptyOutDir: true,
  },
});
