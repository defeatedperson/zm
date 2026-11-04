//! 画廊：扫描壁纸目录、添加（复制进来）、移除、封面落盘。
//! 视频封面由后端 MF 解码生成（mfthumb 模块），图片直接用原图。

use serde::Serialize;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter};

use crate::probe;
use crate::settings;
use crate::wallpaper;

/// WebView2 <video> 能可靠播放的格式
const VIDEO_EXTS: &[&str] = &["mp4", "webm", "m4v"];
/// 静态图片格式（可作为某块屏的静态壁纸）
const IMAGE_EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif"];
/// 封面子目录（位于壁纸目录内，列表时过滤掉）
const THUMB_DIR: &str = ".thumbs";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GalleryEntry {
    pub name: String,
    pub path: String,
    /// "video" | "image"
    pub kind: String,
    pub size_bytes: u64,
    pub modified_ms: Option<u64>,
    pub thumb_path: Option<String>,
    /// 视频元数据（分辨率/帧率/时长），来自 sidecar，探测失败或图片时为 None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<probe::VideoMeta>,
}

/// 是否已有后台回填在跑（防止 list_gallery 重复触发）
static BACKFILL_RUNNING: AtomicBool = AtomicBool::new(false);
/// 是否有导入任务进行中（迁移存储位置等操作的前置检查用）
static IMPORTING: AtomicBool = AtomicBool::new(false);

pub fn is_importing() -> bool {
    IMPORTING.load(Ordering::Relaxed)
}

pub fn is_backfilling() -> bool {
    BACKFILL_RUNNING.load(Ordering::Relaxed)
}

fn meta_path_for(dir: &Path, name: &str) -> PathBuf {
    dir.join(THUMB_DIR).join(format!("{name}.json"))
}

fn read_meta(dir: &Path, name: &str) -> Option<probe::VideoMeta> {
    let text = fs::read_to_string(meta_path_for(dir, name)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 读取条目 sidecar 中的关键帧风险标记（阻止直接应用 / 启动恢复跳过用）。
/// sidecar 缺失或旧格式无该字段时返回 None（视为无风险，不拦截）
pub fn kf_risk_of(app: &AppHandle, path: &str) -> Option<String> {
    let dir = settings::resolve_wallpapers_dir(app).ok()?;
    let name = Path::new(path).file_name()?.to_str()?;
    read_meta(&dir, name).and_then(|m| m.kf_risk)
}

/// 写入条目元数据 sidecar（转码模块替换文件后重建元数据用）
pub fn write_meta(dir: &Path, name: &str, meta: &probe::VideoMeta) -> Result<(), String> {
    fs::create_dir_all(dir.join(THUMB_DIR)).map_err(|e| format!("创建封面目录失败: {e}"))?;
    let json = serde_json::to_string(meta).map_err(|e| e.to_string())?;
    fs::write(meta_path_for(dir, name), json).map_err(|e| format!("写入元数据失败: {e}"))
}

/// 清理条目的封面与元数据缓存（转码替换文件后调用，触发重建）
pub fn invalidate_entry_caches(dir: &Path, name: &str) {
    let thumb = thumb_path_for(dir, name);
    if thumb.exists() {
        let _ = fs::remove_file(thumb);
    }
    let meta_file = meta_path_for(dir, name);
    if meta_file.exists() {
        let _ = fs::remove_file(meta_file);
    }
}

/// 为缺少元数据的存量视频补探测（后台串行），完成后广播刷新
pub fn backfill_meta(app: &AppHandle) {
    if BACKFILL_RUNNING.swap(true, Ordering::Relaxed) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let dir = match settings::resolve_wallpapers_dir(&app) {
            Ok(d) => d,
            Err(_) => {
                BACKFILL_RUNNING.store(false, Ordering::Relaxed);
                return;
            }
        };
        let Ok(items) = fs::read_dir(&dir) else {
            BACKFILL_RUNNING.store(false, Ordering::Relaxed);
            return;
        };
        let mut updated = false;
        for item in items.flatten() {
            let path = item.path();
            if !path.is_file() || !is_video(&path) {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if read_meta(&dir, name).is_some() {
                continue;
            }
            match probe::probe_video(&path) {
                Ok(meta) => {
                    if write_meta(&dir, name, &meta).is_ok() {
                        updated = true;
                        println!("[gallery] 补探测 {name}: {}x{}", meta.width, meta.height);
                    }
                }
                Err(e) => println!("[gallery] 探测 {name} 失败: {e}"),
            }
        }
        BACKFILL_RUNNING.store(false, Ordering::Relaxed);
        if updated {
            let _ = app.emit("gallery:meta-updated", ());
        }
    });
}

fn ext_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
}

fn is_video(path: &Path) -> bool {
    ext_of(path)
        .map(|e| VIDEO_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

/// 是否为受支持的静态图片
pub fn is_image(path: &Path) -> bool {
    ext_of(path)
        .map(|e| IMAGE_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

/// 媒体类型标记（画廊条目与事件分流用）
pub fn media_kind(path: &Path) -> &'static str {
    if is_image(path) {
        "image"
    } else {
        "video"
    }
}

fn thumb_path_for(dir: &Path, name: &str) -> PathBuf {
    dir.join(THUMB_DIR).join(format!("{name}.jpg"))
}

/// 校验 path 位于 dir 内（防止前端传任意路径做删除/写文件）
pub fn ensure_inside(dir: &Path, path: &Path) -> Result<(), String> {
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("解析目录失败: {e}"))?;
    let path = path
        .canonicalize()
        .map_err(|_| "文件不存在或已被移动".to_string())?;
    if path.starts_with(&dir) {
        Ok(())
    } else {
        Err("路径不在壁纸目录内".into())
    }
}

#[tauri::command]
pub fn list_gallery(app: AppHandle) -> Result<Vec<GalleryEntry>, String> {
    let dir = settings::resolve_wallpapers_dir(&app)?;

    let mut entries = Vec::new();
    for item in fs::read_dir(&dir).map_err(|e| format!("读取目录失败: {e}"))? {
        let item = item.map_err(|e| format!("读取目录项失败: {e}"))?;
        let path = item.path();
        if !path.is_file() || (!is_video(&path) && !is_image(&path)) {
            continue;
        }
        let kind = media_kind(&path);
        let fmeta = item.metadata().map_err(|e| format!("读取文件信息失败: {e}"))?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let modified_ms = fmeta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64);
        // 图片直接用原图当封面；视频用 .thumbs 下的截帧
        let thumb = if kind == "image" {
            Some(path.to_string_lossy().into_owned())
        } else {
            thumb_path_for(&dir, &name)
                .exists()
                .then(|| thumb_path_for(&dir, &name).to_string_lossy().into_owned())
        };
        // 视频元数据（sidecar）
        let meta = if kind == "video" {
            read_meta(&dir, &name)
        } else {
            None
        };
        entries.push(GalleryEntry {
            path: path.to_string_lossy().into_owned(),
            name,
            kind: kind.to_string(),
            size_bytes: fmeta.len(),
            modified_ms,
            thumb_path: thumb,
            meta,
        });
    }
    entries.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms));
    Ok(entries)
}

/// 同名冲突时自动追加序号：video.mp4 -> video (2).mp4
fn dedupe_name(dir: &Path, name: &str) -> String {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return name.to_string();
    }
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("video")
        .to_string();
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    for i in 2..1000 {
        let name = format!("{stem} ({i}){ext}");
        if !dir.join(&name).exists() {
            return name;
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    format!("{stem} ({stamp}){ext}")
}

/// 导入进度事件载荷（copy 分块循环中按时间节流广播）
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportProgress {
    copied: u64,
    total: u64,
}

/// 分块复制并广播 import:progress（100ms 节流）。
/// 原实现用 fs::copy 一次性复制大文件，同步命令会阻塞主线程冻结整个应用。
fn copy_with_progress(app: &AppHandle, source: &Path, dest: &Path) -> Result<(), String> {
    let total = fs::metadata(source)
        .map_err(|e| format!("读取源文件失败: {e}"))?
        .len();
    let mut src = fs::File::open(source).map_err(|e| format!("打开源文件失败: {e}"))?;
    let mut dst = fs::File::create(dest).map_err(|e| format!("创建目标文件失败: {e}"))?;
    let mut buf = vec![0u8; 1024 * 1024];
    let mut copied: u64 = 0;
    let mut last_emit = std::time::Instant::now();
    loop {
        let n = src.read(&mut buf).map_err(|e| format!("读取源文件失败: {e}"))?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])
            .map_err(|e| format!("写入目标文件失败: {e}"))?;
        copied += n as u64;
        if last_emit.elapsed() >= std::time::Duration::from_millis(100) {
            let _ = app.emit("import:progress", ImportProgress { copied, total });
            last_emit = std::time::Instant::now();
        }
    }
    let _ = app.emit("import:progress", ImportProgress { copied, total });
    Ok(())
}

fn add_wallpaper_impl(app: &AppHandle, source: &str) -> Result<GalleryEntry, String> {
    if crate::storage::is_migrating() {
        return Err("正在迁移存储位置，请稍候".into());
    }
    let source = PathBuf::from(source);
    if !source.is_file() {
        return Err("源文件不存在".into());
    }
    if !is_video(&source) && !is_image(&source) {
        return Err("仅支持视频（mp4/webm/m4v）或图片（jpg/png/webp/bmp/gif）".into());
    }

    let dir = settings::resolve_wallpapers_dir(app)?;
    let name = source
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("文件名异常")?;
    let name = dedupe_name(&dir, name);
    let dest = dir.join(&name);

    // 分块复制 + 进度广播；失败时清理半成品文件
    if let Err(e) = copy_with_progress(app, &source, &dest) {
        let _ = fs::remove_file(&dest);
        return Err(e);
    }

    // 视频探测元数据并写 sidecar（失败不阻塞导入，后台会补探测）
    let mut meta = None;
    if !is_image(&dest) {
        match probe::probe_video(&dest) {
            Ok(m) => {
                if let Err(e) = write_meta(&dir, &name, &m) {
                    println!("[gallery] 写入元数据失败: {e}");
                }
                meta = Some(m);
            }
            Err(e) => println!("[gallery] 探测 {} 失败: {e}", dest.display()),
        }
    }

    let fmeta = fs::metadata(&dest).map_err(|e| format!("读取文件信息失败: {e}"))?;
    let modified_ms = fmeta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    Ok(GalleryEntry {
        path: dest.to_string_lossy().into_owned(),
        name,
        kind: media_kind(&dest).to_string(),
        size_bytes: fmeta.len(),
        modified_ms,
        // 图片可直接作为封面，立即可见
        thumb_path: is_image(&dest)
            .then(|| dest.to_string_lossy().into_owned()),
        meta,
    })
}

/// 导入素材。必须为 async：复制大文件在同步命令（主线程）执行会冻结整个应用。
#[tauri::command]
pub async fn add_wallpaper(app: AppHandle, source: String) -> Result<GalleryEntry, String> {
    IMPORTING.store(true, Ordering::Relaxed);
    let result = match tauri::async_runtime::spawn_blocking(move || add_wallpaper_impl(&app, &source))
        .await
    {
        Ok(r) => r,
        Err(e) => Err(format!("导入任务调度失败: {e}")),
    };
    IMPORTING.store(false, Ordering::Relaxed);
    result
}

#[tauri::command]
pub fn remove_wallpaper(app: AppHandle, path: String) -> Result<(), String> {
    if crate::storage::is_migrating() {
        return Err("正在迁移存储位置，请稍候".into());
    }
    let dir = settings::resolve_wallpapers_dir(&app)?;
    let target = PathBuf::from(&path);
    ensure_inside(&dir, &target)?;

    if wallpaper::is_wallpaper_in_use(&path) {
        return Err("该视频正在使用中，请先停止对应的屏幕壁纸".into());
    }

    fs::remove_file(&target).map_err(|e| format!("删除文件失败: {e}"))?;
    if let Some(name) = target.file_name().and_then(|n| n.to_str()) {
        let thumb = thumb_path_for(&dir, name);
        if thumb.exists() {
            let _ = fs::remove_file(thumb);
        }
        let meta_file = meta_path_for(&dir, name);
        if meta_file.exists() {
            let _ = fs::remove_file(meta_file);
        }
    }
    Ok(())
}

/// 媒体文件锁：封面生成（MF 解码占用源文件）与转码的「备份原件+替换」
/// 互斥，避免 rename 与打开的文件句柄冲突导致任一方失败
fn media_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// 转码替换文件前获取（与封面生成互斥）；返回守卫供调用方持有
pub fn acquire_media_lock() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    media_lock()
        .lock()
        .map_err(|_| "媒体文件锁异常".to_string())
}

/// 后端生成视频封面：MF 解码目标时间点的帧 → JPEG 落盘，返回封面路径。
/// 取代旧的前端 `<video>` + canvas 截帧（Chromium 对 4K60 High@L5.2 会退
/// 软解，seek 停滞导致封面永远生成中/黑帧，见 mfthumb 模块说明）。
/// 必须 async + spawn_blocking：MF 解码耗时且需 COM 初始化，不能占主线程
#[tauri::command]
pub async fn generate_thumbnail(app: AppHandle, path: String) -> Result<String, String> {
    if crate::storage::is_migrating() {
        return Err("正在迁移存储位置，请稍候".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let dir = settings::resolve_wallpapers_dir(&app)?;
        let target = PathBuf::from(&path);
        ensure_inside(&dir, &target)?;
        let name = target
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("文件名异常")?;
        if !is_video(&target) {
            return Err("仅支持视频生成封面".into());
        }
        let thumb = thumb_path_for(&dir, name);
        fs::create_dir_all(dir.join(THUMB_DIR))
            .map_err(|e| format!("创建封面目录失败: {e}"))?;
        // 持锁生成：转码的备份/替换会等这边的解码句柄释放
        let _guard = acquire_media_lock()?;
        crate::mfthumb::generate(&target, &thumb)?;
        Ok(thumb.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| format!("封面任务调度失败: {e}"))?
}
