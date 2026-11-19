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
    // The main window and the overlay window are separate pages.
    rollupOptions: {
      input: { main: "index.html", overlay: "overlay.html" },
    },
    outDir: "dist",
    emptyOutDir: true,
  },
});
