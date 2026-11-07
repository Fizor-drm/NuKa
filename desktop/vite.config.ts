import { defineConfig } from "vite";

// Tauri は devUrl (http://localhost:1420) をそのまま開くので、ポートを固定して
// 空いていなければ失敗させる。別ポートに逃げると Tauri 側が白画面になる。
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: false,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: "es2022",
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
  },
});
