import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { configDefaults } from "vitest/config";

export default defineConfig({
  plugins: [react()],
  build: {
    minify: false,
  },
  server: {
    port: 42514,
    strictPort: true,
    hmr: {
      port: 42515
    },
    watch: {
      // Rust 源码和编译产物由 Tauri 管理，前端开发服务不监听。
      ignored: ["**/src-tauri/**"]
    }
  },
  clearScreen: false,
  test: {
    exclude: [
      ...configDefaults.exclude,
      ".agents/**",
      ".codex/**",
      "mobile/**",
      "relay/**"
    ]
  }
});
