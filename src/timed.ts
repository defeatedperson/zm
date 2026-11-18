/// 定时切换规则的 API 封装与类型（与后端 timed.rs 的 TimedFile 对应，
/// 持久化于 app_config_dir/timed.json）。
import { invoke } from "@tauri-apps/api/core";

export interface TimedItem {
  /** 素材落盘文件名（不是路径：存储位置迁移后依然有效，运行时由后端拼目录解析） */
  name: string;
  /** 整点时刻 0-23；仅整点模式（mode=0）使用。间隔模式下保留不校验 */
  hour?: number | null;
}

export interface ScreenRule {
  enabled: boolean;
  /** 0 = 整点时刻表；1 = 间隔轮换 */
  mode: 0 | 1;
  /** 间隔分钟（仅 mode=1 生效）；取值见 INTERVAL_OPTIONS，后端白名单校验 */
  intervalMin: number;
  /** 素材列表（数组保序：间隔模式按此顺序轮换） */
  items: TimedItem[];
}

export interface TimedFile {
  version: number;
  /** key = 屏幕 label（硬件稳定标识，与壁纸分配同源） */
  screens: Record<string, ScreenRule>;
}

/** 间隔分钟白名单（与后端 INTERVAL_WHITELIST 是同一份约定，勿单独改） */
export const INTERVAL_OPTIONS = [5, 10, 15, 30, 60, 120] as const;

export const getTimedRules = () => invoke<TimedFile>("get_timed_rules");

/** 整份保存（页面编辑完一次性提交；后端对启用中的规则做兜底校验） */
export const setTimedRules = (rules: TimedFile) =>
  invoke<void>("set_timed_rules", { rules });
