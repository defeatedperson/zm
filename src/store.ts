/// 跨视图共享的状态与操作（轻量 store，不引入 pinia）。
import { computed, reactive } from "vue";
import { ElMessage } from "element-plus";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { getSettings } from "./settings";
import {
  addWallpaper,
  generateThumbnail,
  getScreens,
  listGallery,
  removeWallpaper,
  setWallpaperOn,
  unsetWallpaperOn,
  type GalleryEntry,
  type ScreenInfo,
} from "./gallery";

/** 导航页 id：后续新增功能页在此扩展 */
export type ViewId = "gallery" | "settings";

export interface NavItem {
  id: ViewId;
  label: string;
  hint: string;
  /** 竖条中的分区：top = logo 下方的功能页区，bottom = 底部固定区 */
  position: "top" | "bottom";
}

/** 左侧竖条导航注册表：新增功能页 = 加一项 + NavIcon.vue 补图标 + App.vue 补视图组件 */
export const NAV_ITEMS: NavItem[] = [
  {
    id: "gallery",
    label: "桌面壁纸",
    hint: "点击封面应用到全部屏幕，右键卡片可指定单屏；也可直接拖入文件导入",
    position: "top",
  },
  {
    id: "settings",
    label: "设置",
    hint: "存储位置与偏好设置",
    position: "bottom",
  },
];

export interface Store {
  view: ViewId;
  entries: GalleryEntry[];
  screens: ScreenInfo[];
  busy: boolean;
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
  /** 拖放悬停中（主窗口正接收受支持文件的拖入） */
  dragOver: boolean;
  /** 拖入的待导入文件数（悬停提示用） */
  dragCount: number;
}

export const store = reactive<Store>({
  view: "gallery",
  entries: [],
  screens: [],
  busy: false,
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
  dragOver: false,
  dragCount: 0,
});

/** 全部显示器中的最大物理分辨率（卡片超规格警示与优化弹窗默认档位共用） */
export const maxDisplay = computed(() => {
  let w = 0;
  let h = 0;
  for (const s of store.screens) {
    w = Math.max(w, s.width);
    h = Math.max(h, s.height);
  }
  return { w, h };
});

/** "\\.\DISPLAY1" → "显示器 1"；非标准命名时退回去掉前缀的原始名 */
export const screenShortName = (name: string) => {
  const m = name.match(/^\\\\\.\\DISPLAY(\d+)$/i);
  if (m) return `显示器 ${m[1]}`;
  return name.replace(/^\\\\\.\\/, "");
};

/** 用 Element Plus 的 ElMessage 弹顶部提示；错误态停留更久。
 *  导入期间进度弹窗已承载反馈，不连闪提示 */
export function setStatus(msg: string, error = false) {
  if (store.importing) return;
  ElMessage({
    message: msg,
    type: error ? "error" : "info",
    duration: error ? 6000 : 3000,
  });
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
  // 启动恢复挂载失败（如与 Wallpaper Engine 冲突）：红字提示原因与处理办法
  void listen<string>("wallpaper:attach-error", (event) => {
    setStatus(event.payload, true);
  });
  // 存量视频元数据回填完成后刷新画廊（徽标显示分辨率/帧率）
  void listen("gallery:meta-updated", () => {
    void refresh();
  });
  // 显示器热插拔/布局变化：后端重排壁纸窗口后广播，刷新屏幕状态条
  void listen("wallpaper:screens-changed", () => {
    void refresh();
  });
  // 导入进度：后端分块复制时按 100ms 节流广播
  void listen<{ copied: number; total: number }>("import:progress", (event) => {
    if (!store.importing) return;
    const { copied, total } = event.payload;
    store.importPercent =
      total > 0 ? Math.min(100, Math.floor((copied / total) * 100)) : 0;
  });
  // 开机自启是静默启动，挂载失败可能早于上面的监听注册（事件会丢），
  // 所以再把后端存着的原因拉一次，保证弹窗时状态栏有内容
  try {
    const pending = await invoke<string | null>("get_last_attach_error");
    if (pending) setStatus(pending, true);
  } catch {
    // 拉不到就保持当前状态，不影响主流程
  }
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

/** 每个素材的手动重生成次数（会话级）：传给后端按时长比例轮换采样点，
 *  每次「重新生成封面」换一帧，固定点位撞上坏帧时重试不再复现同一帧 */
const thumbAttempts = new Map<string, number>();

/** 手动重新生成封面（右键菜单）：清除失败标记、复位占位并重新入队。
 *  队列串行执行；正在生成中的条目直接提示，防止重入 */
export function regenThumbnail(path: string) {
  if (store.generating.has(path)) {
    setStatus("该封面正在生成中，请稍候");
    return;
  }
  thumbAttempts.set(path, (thumbAttempts.get(path) ?? 0) + 1);
  const entry = store.entries.find((e) => e.path === path);
  if (entry) entry.thumbPath = null;
  store.thumbFailed = new Set([...store.thumbFailed].filter((p) => p !== path));
  if (!thumbQueue.includes(path)) {
    thumbQueue.push(path);
  }
  void processThumbQueue();
}

/** 重新加载存储目录设置（启动与迁移完成后调用） */
export async function loadStorageDir() {
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

/** 支持导入的扩展名单一来源：拖放过滤、文件对话框 filters 都从这里派生
 *  （与后端 Rust 侧 gallery::VIDEO_EXTS/IMAGE_EXTS 保持一致） */
const VIDEO_EXTS = ["mp4", "webm", "m4v"] as const;
const IMAGE_EXTS = ["jpg", "jpeg", "png", "webp", "bmp", "gif"] as const;
const SUPPORTED_EXTS = new Set<string>([...VIDEO_EXTS, ...IMAGE_EXTS]);

const FILE_FILTERS = [
  { name: "全部支持", extensions: [...SUPPORTED_EXTS] },
  { name: "视频", extensions: [...VIDEO_EXTS] },
  { name: "图片", extensions: [...IMAGE_EXTS] },
];

const isSupportedFile = (p: string) =>
  SUPPORTED_EXTS.has(p.split(".").pop()?.toLowerCase() ?? "");

/** 文件对话框选择导入 */
export async function pickAndAdd() {
  if (store.busy) return;
  const selected = await open({
    multiple: true,
    filters: FILE_FILTERS,
  });
  const paths = Array.isArray(selected)
    ? selected
    : typeof selected === "string"
      ? [selected]
      : [];
  if (paths.length === 0) return;
  await importPaths(paths);
}

/** 导入一组本地文件路径（添加素材按钮与拖放共用入口）。
 *  不支持的扩展名跳过并提示；全部不支持时报错 */
export async function importPaths(paths: string[]) {
  const files = paths.filter(isSupportedFile);
  const skipped = paths.length - files.length;
  if (files.length === 0) {
    setStatus("仅支持视频（mp4/webm/m4v）或图片（jpg/png/webp/bmp/gif）", true);
    return;
  }
  if (store.busy) {
    setStatus("正在处理其他任务，请稍候再导入", true);
    return;
  }
  if (skipped > 0) {
    setStatus(`已跳过 ${skipped} 个不支持的文件`);
  }

  store.busy = true;
  store.importing = true;
  store.importTotal = files.length;
  let ok = 0;
  const errors: string[] = [];
  for (let i = 0; i < files.length; i++) {
    const p = files[i];
    store.importName = p.split(/[\\/]/).pop() ?? p;
    store.importCurrent = i + 1;
    store.importPercent = 0;
    setStatus(`正在导入 (${i + 1}/${files.length})…`);
    try {
      await addWallpaper(p);
      ok += 1;
    } catch (e) {
      // 只记文件名，完整路径会把提示撑爆
      errors.push(`${store.importName}: ${e}`);
    }
  }
  store.importing = false;
  store.busy = false;
  await refresh();
  // 失败多时只展示前两条，避免 ElMessage 被超长文本撑爆
  const failSummary =
    errors.length > 0
      ? `导入完成，${errors.length} 个失败：${errors.slice(0, 2).join("；")}` +
        (errors.length > 2 ? `（等 ${errors.length} 个）` : "")
      : `已导入 ${ok} 个素材`;
  setStatus(failSummary, errors.length > 0);
}

/** 拖放落地（wry 的 onDragDropEvent drop 分支）：过滤后走统一导入流程 */
export function dropFiles(paths: string[]) {
  const files = paths.filter(isSupportedFile);
  if (files.length === 0) {
    setStatus("仅支持视频（mp4/webm/m4v）或图片（jpg/png/webp/bmp/gif）", true);
    return;
  }
  // 任意页面拖入都收：先切到画廊页再导入，进度弹窗与结果提示都能看见
  store.view = "gallery";
  void importPaths(files);
}

/** 拖放悬停提示（enter/leave 分支）：只对包含受支持文件的拖入亮遮罩 */
export function setDragOver(paths: string[] | null) {
  if (paths === null) {
    store.dragOver = false;
    return;
  }
  const count = paths.filter(isSupportedFile).length;
  store.dragCount = count;
  store.dragOver = count > 0;
}

// ---- 应用 / 清除 ----
export async function applyTo(entry: GalleryEntry, label: string | null) {
  if (store.busy) return;
  // 结构异常素材拦截：直接播放会在循环时冻结（Chromium 缺关键帧恢复点）
  if (entry.meta?.kfRisk) {
    setStatus(
      `「${entry.name}」${entry.meta.kfRisk}，已阻止应用。右键该素材选「一键修复」后再使用`,
      true,
    );
    return;
  }
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

async function processThumbQueue() {
  if (thumbRunning) return;
  thumbRunning = true;
  while (thumbQueue.length > 0) {
    const path = thumbQueue.shift()!;
    const entry = store.entries.find((e) => e.path === path);
    if (!entry || entry.thumbPath) continue;
    store.generating = new Set(store.generating).add(path);
    try {
      // 后端 MF 解码截帧：Chromium 对 4K60 High@L5.2 会退软解导致
      // seek 停滞（封面永远"生成中"），MF 管路无此问题（见 mfthumb 模块）
      entry.thumbPath = await generateThumbnail(path, thumbAttempts.get(path) ?? 0);
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
