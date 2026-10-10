//! 应用设置持久化：JSON 存放在系统配置目录（app_config_dir）。
//! 新增设置项只需给 Settings 加字段（serde default 保证旧配置文件兼容）。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use tauri::{AppHandle, Manager};

/// 全屏暂停开关的内存缓存（轮询线程每 2s 读一次，避免反复读文件）
static FULLSCREEN_PAUSE: AtomicBool = AtomicBool::new(true);

fn default_fullscreen_pause() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// 全屏应用占据屏幕时暂停该屏壁纸
    #[serde(default = "default_fullscreen_pause")]
    pub fullscreen_pause: bool,
    /// 每块屏幕的壁纸分配（label -> 视频路径），启动时自动恢复
    #[serde(default)]
    pub wallpaper_assignments: HashMap<String, String>,
    /// 自定义存储根目录（wallpapers/backup 都建在其下）；None = 默认应用数据目录
    #[serde(default)]
    pub storage_root: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fullscreen_pause: true,
            wallpaper_assignments: HashMap::new(),
            storage_root: None,
        }
    }
}

/// 轮询线程使用的开关缓存
pub fn fullscreen_pause_cached() -> bool {
    FULLSCREEN_PAUSE.load(Ordering::Relaxed)
}

/// 串行化 settings.json 的「读-改-写」序列：无锁时并发保存会后写覆盖先写
/// （例如同时应用两块屏的壁纸，可能丢失其中一块的分配）。
fn lock_guard() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// 读取设置（不加锁；公开的 load 内部加锁，「读改写」序列请先持有 lock_guard 再调本函数）
fn load_unlocked(app: &AppHandle) -> Settings {
    let settings: Settings = settings_path(app)
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    FULLSCREEN_PAUSE.store(settings.fullscreen_pause, Ordering::Relaxed);
    settings
}

pub fn load(app: &AppHandle) -> Settings {
    let _guard = lock_guard();
    load_unlocked(app)
}

/// 返回给前端的视图：设置 + 解析后的实际目录。
/// 注意 settings 使用 serde(flatten) 扁平展开，视图自身字段不能与 Settings 字段重名。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    #[serde(flatten)]
    pub settings: Settings,
    pub resolved_wallpapers_dir: String,
    /// 默认存储根目录（应用数据目录），供迁移后的残留清理入口使用
    pub default_storage_root: String,
    /// 默认位置是否仍存在 wallpapers/backup 目录
    pub default_dirs_exist: bool,
    /// 自定义存储位置是否当前实际生效（配置了且目录可用；
    /// 磁盘未接入等回退场景下为 false，此时默认目录正在被使用，不可清理）
    pub custom_storage_active: bool,
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_config_dir().map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

/// 原子写入：先写临时文件再 rename 替换（Windows 下 rename 会覆盖已存在目标），
/// 避免写一半崩溃/断电导致 settings.json 损坏、全部壁纸分配丢失。
/// 不加锁；「读改写」序列的调用方应持有 lock_guard。
fn save_unlocked(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("写入配置失败: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("替换配置失败: {e}"))
}

/// 解析壁纸存放目录（自定义根目录下的 wallpapers，未配置时为默认位置），目录不存在时自动创建
pub fn resolve_wallpapers_dir(app: &AppHandle) -> Result<PathBuf, String> {
    resolve_storage_subdir(app, "wallpapers", "壁纸目录")
}

/// 解析备份目录（自定义根目录下的 backup，未配置时为默认位置），目录不存在时自动创建
pub fn resolve_backup_dir(app: &AppHandle) -> Result<PathBuf, String> {
    resolve_storage_subdir(app, "backup", "备份目录")
}

/// 存储子目录统一解析：配置了自定义根目录且该目录可用时用 <root>/<name>，
/// 否则用默认位置。自定义根目录不存在（如移动硬盘未接入）时自动回退默认位置，
/// 保证画廊与壁纸功能不至于整体不可用。
fn resolve_storage_subdir(app: &AppHandle, name: &str, label: &str) -> Result<PathBuf, String> {
    let default_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join(name);
    let dir = match load(app).storage_root {
        Some(root) if PathBuf::from(&root).is_dir() => PathBuf::from(root).join(name),
        Some(root) => {
            println!("[settings] 自定义存储位置不可用，回退默认位置：{root}");
            default_dir
        }
        None => default_dir,
    };
    if !dir.exists() {
        fs::create_dir_all(&dir).map_err(|e| format!("创建{label}失败: {e}"))?;
    }
    Ok(dir)
}

/// 自定义存储位置是否当前实际生效：配置了且根目录可用。
/// 磁盘未接入等回退场景下返回 false（此时实际使用默认位置）。
pub fn using_custom_root(app: &AppHandle) -> bool {
    match load(app).storage_root {
        Some(root) => PathBuf::from(root).is_dir(),
        None => false,
    }
}

/// 迁移切换步骤：设置存储根目录，并清掉指向旧壁纸目录的分配（其路径已随迁移失效）。
/// 整个读改写序列持锁，防止与其他写操作交错。
pub fn apply_storage_root(
    app: &AppHandle,
    root: Option<String>,
    old_wallpapers: &Path,
) -> Result<(), String> {
    let _guard = lock_guard();
    let mut settings = load_unlocked(app);
    settings.storage_root = root;
    // 大小写不敏感比较；追加分隔符避免 "wallpapers2" 误匹配 "wallpapers" 前缀
    let old = old_wallpapers.to_string_lossy().to_ascii_lowercase();
    settings
        .wallpaper_assignments
        .retain(|_, p| {
            let p = p.to_ascii_lowercase();
            !(p == old || p.starts_with(&format!("{old}\\")))
        });
    save_unlocked(app, &settings)
}

fn view(app: &AppHandle, settings: &Settings) -> Result<SettingsView, String> {
    let default_root = app.path().app_local_data_dir().map_err(|e| e.to_string())?;
    let default_dirs_exist = default_root.join("wallpapers").exists()
        || default_root.join("backup").exists();
    Ok(SettingsView {
        settings: settings.clone(),
        resolved_wallpapers_dir: resolve_wallpapers_dir(app)?.to_string_lossy().into_owned(),
        default_storage_root: default_root.to_string_lossy().into_owned(),
        default_dirs_exist,
        custom_storage_active: using_custom_root(app),
    })
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Result<SettingsView, String> {
    let settings = load(&app);
    view(&app, &settings)
}

/// 更新并持久化每屏壁纸分配（label -> path）。整个读改写序列持锁，防止并发覆盖。
pub fn update_assignments<F>(app: &AppHandle, f: F)
where
    F: FnOnce(&mut HashMap<String, String>),
{
    let _guard = lock_guard();
    let mut settings = load_unlocked(app);
    f(&mut settings.wallpaper_assignments);
    if let Err(e) = save_unlocked(app, &settings) {
        println!("[settings] 保存壁纸分配失败: {e}");
    }
}

/// 在资源管理器中打开目录（Rust 侧调用，不受前端 opener 作用域限制）。
/// 仅允许打开真实存在的目录：防止前端被利用来用系统默认程序启动任意文件。
#[tauri::command]
pub fn open_path_in_explorer(path: String) -> Result<(), String> {
    if !PathBuf::from(&path).is_dir() {
        return Err("路径不是有效目录".into());
    }
    tauri_plugin_opener::open_path(&path, None::<&str>).map_err(|e| e.to_string())
}

/// 用系统默认浏览器打开网址。仅允许 http/https，防止前端调用任意协议。
#[tauri::command]
pub fn open_url_in_browser(url: String) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        return Err("仅支持 http/https 链接".into());
    }
    tauri_plugin_opener::open_url(&url, None::<&str>).map_err(|e| e.to_string())
}

/// 设置「全屏时暂停壁纸」开关（读改写持锁）
#[tauri::command]
pub fn set_fullscreen_pause(app: AppHandle, enabled: bool) -> Result<(), String> {
    let _guard = lock_guard();
    let mut settings = load_unlocked(&app);
    settings.fullscreen_pause = enabled;
    save_unlocked(&app, &settings)?;
    FULLSCREEN_PAUSE.store(enabled, Ordering::Relaxed);
    Ok(())
}
