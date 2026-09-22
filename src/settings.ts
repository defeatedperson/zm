/// 前端设置类型与操作封装，集中对接 Rust 的 settings 命令。
import { invoke } from "@tauri-apps/api/core";

export interface Settings {
  fullscreenPause: boolean;
}

export interface SettingsView {
  settings: Settings;
  resolvedWallpapersDir: string;
}

export async function getSettings(): Promise<SettingsView> {
  return invoke<SettingsView>("get_settings");
}

/** 设置「全屏时暂停壁纸」开关 */
export async function setFullscreenPause(enabled: boolean): Promise<void> {
  return invoke<void>("set_fullscreen_pause", { enabled });
}

/** 获取备份目录（自动创建） */
export async function getBackupDir(): Promise<string> {
  return invoke<string>("get_backup_dir");
}

/** 用系统默认浏览器打开网址（仅 http/https） */
export async function openUrlInBrowser(url: string): Promise<void> {
  return invoke<void>("open_url_in_browser", { url });
}