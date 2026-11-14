/// 无边框窗口的窗口级操作：句柄获取、最大化状态、缩放热区方向表。
/// TitleBar / ResizeHandles 共用，避免逻辑在组件里重复。
import { ref } from "vue";
import {
  getCurrentWindow,
  type Window as TauriWindow,
} from "@tauri-apps/api/window";

// 纯浏览器预览（vite dev 单独打开）没有 Tauri IPC，getCurrentWindow 会在
// 模块顶层抛错导致白屏——捕获后仅禁用窗口按钮，其余 UI 照常渲染
export const appWindow = (() => {
  try {
    return getCurrentWindow();
  } catch {
    return null;
  }
})();

export type ResizeDir = Parameters<TauriWindow["startResizeDragging"]>[0];

// 无边框窗口失去系统边缘缩放：用 8 向透明热区手动发起缩放。
// 最大化/还原态下系统不接受边缘缩放，热区整体隐藏
export const RESIZE_EDGES: { dir: ResizeDir; cls: string }[] = [
  { dir: "North", cls: "rs-n" },
  { dir: "South", cls: "rs-s" },
  { dir: "East", cls: "rs-e" },
  { dir: "West", cls: "rs-w" },
  { dir: "NorthEast", cls: "rs-ne" },
  { dir: "NorthWest", cls: "rs-nw" },
  { dir: "SouthEast", cls: "rs-se" },
  { dir: "SouthWest", cls: "rs-sw" },
];

export const maximized = ref(false);

export async function syncMaximized() {
  if (!appWindow) return;
  try {
    maximized.value = await appWindow.isMaximized();
  } catch {
    // 查询失败保持原状
  }
}

export async function toggleMax() {
  if (!appWindow) return;
  try {
    await appWindow.toggleMaximize();
    await syncMaximized();
  } catch {
    // 窗口操作失败不做提示：按钮态会随下次同步纠正
  }
}

export function startResize(dir: ResizeDir) {
  if (!appWindow) return;
  void appWindow.startResizeDragging(dir).catch(() => {});
}

/** 启动时调用：同步一次最大化状态，并跟随系统快捷键（Win+↑ 等）引起的变化 */
export function initWindowState() {
  if (!appWindow) return;
  void syncMaximized();
  void appWindow.listen("tauri://resize", () => {
    // 拖拽缩放时 resize 事件逐帧触发，去抖后再查最大化状态，避免 IPC 洪泛
    clearTimeout(resizeSyncTimer);
    resizeSyncTimer = setTimeout(() => {
      void syncMaximized();
    }, 150);
  });
}

let resizeSyncTimer: ReturnType<typeof setTimeout> | undefined;
