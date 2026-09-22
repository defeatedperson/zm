/// 跨视图共享的状态与操作（轻量 store，不引入 pinia）。
import { reactive } from "vue";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { getSettings } from "./settings";
import {
  addWallpaper,
  getScreens,
  listGallery,
  removeWallpaper,
  saveThumbnail,
  setWallpaperOn,
  unsetWallpaperOn,
  type GalleryEntry,
  type ScreenInfo,
} from "./gallery";

export interface Store {
  view: "gallery" | "settings";
  entries: GalleryEntry[];
  screens: ScreenInfo[];
  busy: boolean;
  status: string;
  statusError: boolean;
  storageDir: string;
  appVersion: string;
  /** 正在生成封面的视频路径 */
  generating: Set<string>;
  /** 本次会话中封面生成失败的视频路径（不重试） */
  thumbFailed: Set<string>;
  /** 用户手动暂停（仅冻结画面） */
  userPaused: boolean;
  /** 导入进度弹窗状态 */
  importing: boolean;
  importCurrent: number;
  importTotal: number;
  importName: string;
  importPercent: number;
}

export const store = reactive<Store>({
  view: "gallery",
  entries: [],
  screens: [],
  busy: false,
  status: "就绪",
  statusError: false,
  storageDir: "",
  appVersion: "",
  generating: new Set(),
  thumbFailed: new Set(),
  userPaused: false,
  importing: false,
  importCurrent: 0,
  importTotal: 0,
  importName: "",
  importPercent: 0,
});

/** "\\.\DISPLAY1" → "显示器 1"；非标准命名时退回去掉前缀的原始名 */
export const screenShortName = (name: string) => {
  const m = name.match(/^\\\\\.\\DISPLAY(\d+)$/i);
  if (m) return `显示器 ${m[1]}`;
  return name.replace(/^\\\\\.\\/, "");
};

export function setStatus(msg: string, error = false) {
  store.status = msg;
  store.statusError = error;
}

/** 启动初始化：加载画廊/屏幕/设置/版本号，监听暂停状态广播 */
export async function initApp() {
  await Promise.all([refresh(), loadStorageDir()]);
  getVersion()
    .then((v) => (store.appVersion = v))
    .catch(() => {});
  try {
    store.userPaused = await invoke<boolean>("get_user_paused");
  } catch {
    // 保持默认未暂停
  }
  // 托盘/壁纸窗口侧的暂停切换会广播回来，主窗口同步 UI
  void listen<boolean>("wallpaper:user-paused", (event) => {
    store.userPaused = event.payload;
  });
  // 存量视频元数据回填完成后刷新画廊（徽标显示分辨率/帧率）
  void listen("gallery:meta-updated", () => {
    void refresh();
  });
  // 导入进度：后端分块复制时按 100ms 节流广播
  void listen<{ copied: number; total: number }>("import:progress", (event) => {
    if (!store.importing) return;
    const { copied, total } = event.payload;
    store.importPercent =
      total > 0 ? Math.min(100, Math.floor((copied / total) * 100)) : 0;
  });
}

/** 切换用户暂停（仅冻结画面，不卸载壁纸） */
export async function toggleUserPaused() {
  const next = !store.userPaused;
  try {
    await invoke<void>("set_user_paused", { paused: next });
    store.userPaused = next;
    setStatus(next ? "壁纸已暂停（画面冻结）" : "壁纸已恢复播放");
  } catch (e) {
    setStatus(`操作失败：${e}`, true);
  }
}

async function loadStorageDir() {
  try {
    store.storageDir = (await getSettings()).resolvedWallpapersDir;
  } catch (e) {
    console.error("加载设置失败", e);
  }
}

export async function refresh() {
  try {
    [store.entries, store.screens] = await Promise.all([
      listGallery(),
      getScreens(),
    ]);
    enqueueThumbs();
  } catch (e) {
    setStatus(`加载画廊失败：${e}`, true);
  }
}

// ---- 添加素材（视频/图片）----
export async function pickAndAdd() {
  if (store.busy) return;
  const selected = await open({
    multiple: true,
    filters: [
      { name: "全部支持", extensions: ["mp4", "webm", "m4v", "jpg", "jpeg", "png", "webp", "bmp", "gif"] },
      { name: "视频", extensions: ["mp4", "webm", "m4v"] },
      { name: "图片", extensions: ["jpg", "jpeg", "png", "webp", "bmp", "gif"] },
    ],
  });
  const paths = Array.isArray(selected)
    ? selected
    : typeof selected === "string"
      ? [selected]
      : [];
  if (paths.length === 0) return;

  store.busy = true;
  store.importing = true;
  store.importTotal = paths.length;
  let ok = 0;
  const errors: string[] = [];
  for (let i = 0; i < paths.length; i++) {
    const p = paths[i];
    store.importCurrent = i + 1;
    store.importName = p.split(/[\\/]/).pop() ?? p;
    store.importPercent = 0;
    setStatus(`正在导入 (${i + 1}/${paths.length})…`);
    try {
      await addWallpaper(p);
      ok += 1;
    } catch (e) {
      errors.push(`${p}: ${e}`);
    }
  }
  store.importing = false;
  store.busy = false;
  await refresh();
  setStatus(
    errors.length > 0
      ? `导入完成，${errors.length} 个失败：${errors.join("；")}`
      : `已导入 ${ok} 个素材`,
    errors.length > 0,
  );
}

// ---- 应用 / 清除 ----
export async function applyTo(entry: GalleryEntry, label: string | null) {
  if (store.busy) return;
  store.busy = true;
  try {
    await setWallpaperOn(entry.path, label);
    await refresh();
    const target = label
      ? screenShortName(
          store.screens.find((s) => s.label === label)?.name ?? label,
        )
      : "全部屏幕";
    setStatus(`已将「${entry.name}」应用到 ${target}`);
  } catch (e) {
    setStatus(`设置失败：${e}`, true);
  } finally {
    store.busy = false;
  }
}

export async function clearScreen(label: string) {
  try {
    await unsetWallpaperOn(label);
    await refresh();
    setStatus("已恢复该屏幕的静态壁纸");
  } catch (e) {
    setStatus(`清除失败：${e}`, true);
  }
}

export async function clearAll() {
  try {
    await unsetWallpaperOn(null);
    await refresh();
    setStatus("已恢复所有屏幕的静态壁纸");
  } catch (e) {
    setStatus(`清除失败：${e}`, true);
  }
}

export async function removeGalleryItem(entry: GalleryEntry) {
  try {
    await removeWallpaper(entry.path);
    await refresh();
    setStatus(`已移除「${entry.name}」`);
  } catch (e) {
    setStatus(`移除失败：${e}`, true);
  }
}

// ---- 存储位置 ----
/** 在资源管理器中打开目录（Rust 侧调用 opener，避开前端作用域限制） */
export async function openDirInExplorer(path: string) {
  try {
    await invoke("open_path_in_explorer", { path });
    setStatus(`已打开目录：${path}`);
  } catch (e) {
    setStatus(`打开目录失败：${e}`, true);
  }
}

// ---- 封面生成队列（逐个串行，避免同时解码多个视频） ----
const thumbQueue: string[] = [];
let thumbRunning = false;

function enqueueThumbs() {
  for (const e of store.entries) {
    if (
      !e.thumbPath &&
      !store.thumbFailed.has(e.path) &&
      !thumbQueue.includes(e.path) &&
      !store.generating.has(e.path)
    ) {
      thumbQueue.push(e.path);
    }
  }
  void processThumbQueue();
}

function captureFrame(entry: GalleryEntry): Promise<string> {
  return new Promise((resolve, reject) => {
    const video = document.createElement("video");
    video.muted = true;
    video.preload = "auto";
    // 必须：asset 协议是跨域资源，不带 crossOrigin 时 canvas 会被污染，
    // toDataURL 抛 SecurityError，表现为"无法生成封面"
    video.crossOrigin = "anonymous";
    const cleanup = () => {
      video.removeAttribute("src");
      video.load();
    };
    video.addEventListener("loadedmetadata", () => {
      // 取开头 1 秒或 10% 处的帧
      video.currentTime = Math.min(1, (video.duration || 0) * 0.1);
    });
    video.addEventListener("seeked", () => {
      try {
        const canvas = document.createElement("canvas");
        const width = Math.min(480, video.videoWidth || 480);
        canvas.width = width;
        canvas.height =
          Math.round(width * (video.videoHeight / video.videoWidth)) || 270;
        const ctx = canvas.getContext("2d");
        if (!ctx) throw new Error("canvas 不可用");
        ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
        resolve(canvas.toDataURL("image/jpeg", 0.82));
      } catch (err) {
        reject(err);
      } finally {
        cleanup();
      }
    });
    video.addEventListener("error", () => {
      cleanup();
      reject(new Error("视频无法解码"));
    });
    video.src = convertFileSrc(entry.path);
  });
}

async function processThumbQueue() {
  if (thumbRunning) return;
  thumbRunning = true;
  while (thumbQueue.length > 0) {
    const path = thumbQueue.shift()!;
    const entry = store.entries.find((e) => e.path === path);
    if (!entry || entry.thumbPath) continue;
    store.generating = new Set(store.generating).add(path);
    try {
      entry.thumbPath = await saveThumbnail(path, await captureFrame(entry));
    } catch {
      store.thumbFailed = new Set(store.thumbFailed).add(path);
    } finally {
      const next = new Set(store.generating);
      next.delete(path);
      store.generating = next;
    }
  }
  thumbRunning = false;
}
