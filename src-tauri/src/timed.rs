//! 定时切换规则持久化：timed.json（app_config_dir 下，与 settings.json 同目录）。
//!
//! 本模块 D1 只负责结构定义、校验与读写；调度引擎（无状态幂等判定 + 降级自愈）
//! 在 D2 接入（见项目记忆「定时切换设计共识」）。
//!
//! 关键设计：
//! - 素材用**落盘文件名**而非完整路径：存储位置迁移整目录搬运、文件名不变，
//!   规则天然免疫（与 .thumbs sidecar 的命名约定同族）；运行时由
//!   resolve_wallpapers_dir().join(name) 解析成真实路径。
//! - hour 是可选字段：间隔轮换（mode=1）忽略它，但序列化时保留——用户在
//!   两种模式间来回切换时不丢已配置的时刻。
//! - 文件缺失视为首次使用（空规则）；文件存在但解析失败必须报错而不是静默
//!   回默认——规则是用户逐条编辑的数据，静默清空等于丢数据。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use tauri::{AppHandle, Manager};

use crate::settings;

/// 模式：整点时刻表（每天在指定整点切换到对应素材）
pub const MODE_HOUR: u8 = 0;
/// 模式：间隔轮换（每隔 interval_min 分钟切换到列表中的下一个素材）
pub const MODE_INTERVAL: u8 = 1;

/// 间隔分钟白名单（与前端下拉共用一份约定；后端校验兜底）
pub const INTERVAL_WHITELIST: &[u32] = &[5, 10, 15, 30, 60, 120];

fn default_version() -> u32 {
    1
}

/// 单条素材条目
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimedItem {
    /// 素材落盘文件名（wallpapers 目录第一层，同目录天然唯一）
    pub name: String,
    /// 整点时刻 0-23；仅 mode=0 使用。间隔模式下保留不校验（模式回切不丢配置）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hour: Option<u8>,
}

/// 单块屏幕的定时规则
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenRule {
    pub enabled: bool,
    /// MODE_HOUR / MODE_INTERVAL
    pub mode: u8,
    /// 间隔分钟（仅 mode=1 生效，白名单校验）
    #[serde(default)]
    pub interval_min: u32,
    /// 素材列表（数组保序：间隔模式按此顺序轮换）
    #[serde(default)]
    pub items: Vec<TimedItem>,
}

/// timed.json 全文。BTreeMap 让序列化按键排序，文件内容确定（便于肉眼 diff）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimedFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub screens: BTreeMap<String, ScreenRule>,
}

impl Default for TimedFile {
    fn default() -> Self {
        Self {
            version: default_version(),
            screens: BTreeMap::new(),
        }
    }
}

/// 串行化 timed.json 的读-改-写（与 settings::lock_guard 同款；中毒用
/// into_inner 恢复，避免一次 panic 让规则读写永久失败）
fn lock_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn timed_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("timed.json"))
}

/// 读取（不加锁；读改写序列请先持有 lock_guard）。调度器每 tick 也走这里：
/// 只读 + 原子 rename 写入保证读到的是旧或新的完整文件
pub(crate) fn load_unlocked(app: &AppHandle) -> Result<TimedFile, String> {
    let path = timed_path(app)?;
    if !path.exists() {
        return Ok(TimedFile::default());
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("读取定时规则失败: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("定时规则文件损坏: {e}"))
}

/// 原子写入（tmp + rename，同 settings.json 的防半写策略）；不加锁，调用方持有 lock_guard
fn save_unlocked(app: &AppHandle, file: &TimedFile) -> Result<(), String> {
    let path = timed_path(app)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("写入定时规则失败: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("替换定时规则失败: {e}"))
}

/// 保存前校验。只校验**启用中**的规则：停用规则允许带失效素材名/缺时刻保存
/// （关掉开关必须永远可行，否则会出现「坏规则关不掉」的死局）。
/// 错误消息面向用户（前端 toast 直接展示），label 用短名交由前端拼接，这里
/// 只保证后端兜底时信息完整。
fn validate(app: &AppHandle, file: &TimedFile) -> Result<(), String> {
    if file.version != 1 {
        return Err(format!("不支持的规则文件版本：{}", file.version));
    }
    let wallpapers_dir = settings::resolve_wallpapers_dir(app)?;
    for (label, rule) in &file.screens {
        if rule.mode != MODE_HOUR && rule.mode != MODE_INTERVAL {
            return Err(format!("{label}：模式值无效（{}）", rule.mode));
        }
        if !rule.enabled {
            continue;
        }
        if rule.items.is_empty() {
            return Err(format!("{label}：已开启定时但素材列表为空"));
        }
        if rule.items.iter().any(|i| i.name.trim().is_empty()) {
            return Err(format!("{label}：存在空素材名"));
        }
        // 文件名安全护栏：真实落盘名不含路径分隔符（画廊条目都是目录第一层
        // 文件），含分隔符的 name 会让后续 join 逃出壁纸目录
        if let Some(bad) = rule.items.iter().find(|i| i.name.contains(['\\', '/'])) {
            return Err(format!(
                "{label}：素材名异常（不应包含路径分隔符）：{}",
                bad.name
            ));
        }
        if rule.mode == MODE_HOUR {
            let mut hours: Vec<u8> = Vec::new();
            for item in &rule.items {
                let Some(h) = item.hour else {
                    return Err(format!("{label}：素材「{}」缺少切换时刻", item.name));
                };
                if h > 23 {
                    return Err(format!("{label}：时刻 {h} 超出 0-23"));
                }
                if hours.contains(&h) {
                    return Err(format!("{label}：{} 点已有素材，同一屏幕的时刻不能重复", h));
                }
                hours.push(h);
            }
        } else if !INTERVAL_WHITELIST.contains(&rule.interval_min) {
            return Err(format!(
                "{label}：切换间隔必须在 {} 分钟之内选一",
                INTERVAL_WHITELIST
                    .iter()
                    .map(|m| m.to_string())
                    .collect::<Vec<_>>()
                    .join("/")
            ));
        }
        // 素材存在性：名字来自画廊（前端只提供现存素材），这里兜住
        // 「保存后才被删/改名」的漏网。逐个 join 而非 canonicalize：
        // 文件名不含路径分隔符（画廊条目都是目录第一层文件），join 即最终路径
        let missing: Vec<&str> = rule
            .items
            .iter()
            .map(|i| i.name.as_str())
            .filter(|n| !wallpapers_dir.join(n).is_file())
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "{label}：素材不存在（已删除或被移动）：{}",
                missing.join("、")
            ));
        }
    }
    Ok(())
}

/// 读取启用中的定时规则屏幕 label 集合（手动应用/清除守卫、restore 跳过用）。
/// 文件读取失败按空处理：规则文件异常不该连带阻塞手动壁纸操作
pub fn enabled_labels(app: &AppHandle) -> HashSet<String> {
    load_unlocked(app)
        .map(|f| {
            f.screens
                .into_iter()
                .filter(|(_, r)| r.enabled)
                .map(|(l, _)| l)
                .collect()
        })
        .unwrap_or_default()
}

/// 素材名是否被某块启用定时的屏幕引用（删除素材守卫用），返回去掉窗口
/// 前缀的短屏名供错误消息展示。停用规则不算——停用即惰性，与
/// 「关掉开关永远可行」的校验原则一致
pub fn enabled_rule_using(app: &AppHandle, file_name: &str) -> Option<String> {
    let file = load_unlocked(app).ok()?;
    file.screens
        .into_iter()
        .find(|(_, r)| r.enabled && r.items.iter().any(|i| i.name == file_name))
        .map(|(label, _)| {
            label
                .trim_start_matches(crate::wallpaper::WALLPAPER_PREFIX)
                .to_string()
        })
}

/// 读取定时规则。async + spawn_blocking：磁盘 IO 不占主线程（项目约定）
#[tauri::command]
pub async fn get_timed_rules(app: AppHandle) -> Result<TimedFile, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = lock_guard();
        load_unlocked(&app)
    })
    .await
    .map_err(|e| format!("读取定时规则失败: {e}"))?
}

/// 整份保存（页面编辑完一次性提交）。先校验后写入，失败不动原文件。
/// async + spawn_blocking：校验含逐文件存在性检查与写盘
#[tauri::command]
pub async fn set_timed_rules(app: AppHandle, rules: TimedFile) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        validate(&app, &rules)?;
        let _guard = lock_guard();
        // 旧文件损坏时按空处理：保存本身即修复路径（validate 已确保新内容合法）
        let old = load_unlocked(&app).unwrap_or_default();
        save_unlocked(&app, &rules)?;
        adopt_disabled_screens(&app, &old, &rules);
        Ok(())
    })
    .await
    .map_err(|e| format!("保存定时规则失败: {e}"))?
}

/// 关闭定时开关的屏幕善后：若该屏此刻正播放旧规则中的素材，把这条分配补写进
/// settings.json——定时应用不写手动分配，不补的话「关掉开关」当下画面正常、
/// 重启后却因分配为空凭空消失。播放中的内容不属于旧规则则不动（守卫已拦住
/// 手动应用，理论上不会出现，防御性跳过）。规则被整条删除同样走这里。
fn adopt_disabled_screens(app: &AppHandle, old: &TimedFile, new: &TimedFile) {
    for (label, old_rule) in &old.screens {
        if !old_rule.enabled {
            continue;
        }
        // 新文件里该屏不再启用（关了开关，或整条规则被删/清空）
        if new.screens.get(label).is_some_and(|r| r.enabled) {
            continue;
        }
        let Some(current) = crate::wallpaper::current_media(label) else {
            continue;
        };
        let in_old_rule = std::path::Path::new(&current)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| old_rule.items.iter().any(|i| i.name == n));
        if !in_old_rule {
            continue;
        }
        // 有效性复核：内存态可能滞后于文件系统（调度器 2s 一拍，删除/打上
        // kfRisk 可能发生在最后一次收敛之后）。无效素材不能转手动分配
        // （重启恢复、托盘重应用都会再次被拦或跳过），直接卸载回静态壁纸
        let valid = std::path::Path::new(&current).is_file()
            && crate::gallery::kf_risk_of(app, &current).is_none();
        if valid {
            crate::settings::update_assignments(app, |m| {
                m.insert(label.clone(), current.clone());
            });
            println!("[timed] {label} 关闭定时，当前素材转为手动分配");
        } else {
            crate::wallpaper::sched_teardown(app, label);
            println!("[timed] {label} 关闭定时，当前素材已失效，恢复静态壁纸");
        }
    }
}
