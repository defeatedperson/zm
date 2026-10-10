/// 前端设置类型与操作封装，集中对接 Rust 的 settings 命令。
import { invoke } from "@tauri-apps/api/core";

export interface Settings {
  fullscreenPause: boolean;
  /** 自定义存储根目录（wallpapers/backup 都建在其下），未配置为 null */
  storageRoot: string | null;
}

/// get_settings 返回扁平结构（Rust 侧 Settings 经 serde(flatten) 展开）
export interface SettingsView extends Settings {
  resolvedWallpapersDir: string;
  /** 默认存储根目录（应用数据目录），供迁移后的残留清理入口使用 */
  defaultStorageRoot: string;
  /** 默认位置是否仍存在 wallpapers/backup 目录 */
  defaultDirsExist: boolean;
  /** 自定义存储位置是否当前实际生效（磁盘未接入回退默认时为 false） */
  customStorageActive: boolean;
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

export interface MigrationResult {
  /** 旧目录清理失败的原因（迁移本身已成功） */
  warning: string | null;
}

/** 更改存储位置（整体迁移 wallpapers + backup，需先关闭所有壁纸） */
export async function migrateStorage(newRoot: string): Promise<MigrationResult> {
  return invoke<MigrationResult>("migrate_storage", { newRoot });
}

/** 一键清理默认存储位置的 wallpapers 与 backup 残留（仅自定义位置生效后可用） */
export async function deleteDefaultDirs(): Promise<void> {
  return invoke<void>("delete_default_dirs");
}

/** 用系统默认浏览器打开网址（仅 http/https） */
export async function openUrlInBrowser(url: string): Promise<void> {
  return invoke<void>("open_url_in_browser", { url });
}
