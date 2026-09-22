/// 画廊与屏幕状态的 API 封装。
import { invoke } from "@tauri-apps/api/core";

/// 视频元数据（由 Rust 侧 MF 探测，sidecar 持久化）
export interface VideoMeta {
  width: number;
  height: number;
  fpsNum: number;
  fpsDen: number;
  durationMs: number;
}

export interface GalleryEntry {
  name: string;
  path: string;
  /// "video" | "image"
  kind: string;
  sizeBytes: number;
  modifiedMs: number | null;
  thumbPath: string | null;
  meta?: VideoMeta | null;
}

export interface ScreenInfo {
  label: string;
  name: string;
  width: number;
  height: number;
  wallpaperPath: string | null;
}

export const listGallery = () => invoke<GalleryEntry[]>("list_gallery");

export const addWallpaper = (source: string) =>
  invoke<GalleryEntry>("add_wallpaper", { source });

export const removeWallpaper = (path: string) =>
  invoke<void>("remove_wallpaper", { path });

export const saveThumbnail = (path: string, dataUrl: string) =>
  invoke<string>("save_thumbnail", { path, dataUrl });

export const getScreens = () => invoke<ScreenInfo[]>("get_screens");

/** label 为 null 时应用到全部屏幕 */
export const setWallpaperOn = (path: string, label: string | null) =>
  invoke<void>("set_wallpaper_on", { path, label });

/** label 为 null 时清除全部屏幕 */
export const unsetWallpaperOn = (label: string | null) =>
  invoke<void>("unset_wallpaper_on", { label });

/** 启动视频优化转码（后台执行，进度经 transcode:progress/done 事件广播）。
 * targetWidth/Height 传 0 表示保持原始；fpsNum/fpsDen 传 0 表示保持原始帧率。 */
export const optimizeStart = (
  path: string,
  targetWidth: number,
  targetHeight: number,
  fpsNum: number,
  fpsDen: number,
) =>
  invoke<void>("optimize_start", {
    path,
    targetWidth,
    targetHeight,
    fpsNum,
    fpsDen,
  });

/** 取消当前转码 */
export const optimizeCancel = () => invoke<void>("optimize_cancel");
