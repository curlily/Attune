import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Tauri expects a fixed port and loads the built files from `dist`.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: { ignored: ["**/src-tauri/**", "**/target/**", "**/core/**", "**/server/**", "**/protocol/**"] },
  },
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
  },
});
