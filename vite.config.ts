import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";
import { fileURLToPath, URL } from "node:url";
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [vue()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  // 端口避开 49xxx-5xxx 段：Windows 的 Hyper-V/WSL 会随机保留一批动态端口
  // （netsh interface ipv4 show excludedportrange 可查），5173 曾被整段保留导致
  // vite 启动报 EACCES；14300 不在常见保留段内。须与 tauri.conf.json 的 devUrl 一致
  server: {
    port: 14300,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 14301,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
  // 多页面入口：main = 控制面板窗口，wallpaper = 壁纸窗口
  build: {
    target: "es2021",
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL("./index.html", import.meta.url)),
        wallpaper: fileURLToPath(new URL("./wallpaper.html", import.meta.url)),
      },
    },
  },
}));
