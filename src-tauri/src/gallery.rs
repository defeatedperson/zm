//! 画廊：扫描壁纸目录、添加（复制进来）、移除、封面落盘。
//! 视频封面由前端 <video> + canvas 截帧，本模块只负责文件读写。

use base64::Engine;
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
const THUMB_DATA_URL_PREFIX: &str = "data:image/jpeg;base64,";

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

fn meta_path_for(dir: &Path, name: &str) -> PathBuf {
    dir.join(THUMB_DIR).join(format!("{name}.json"))
}

fn read_meta(dir: &Path, name: &str) -> Option<probe::VideoMeta> {
    let text = fs::read_to_string(meta_path_for(dir, name)).ok()?;
    serde_json::from_str(&text).ok()
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
    tauri::async_runtime::spawn_blocking(move || add_wallpaper_impl(&app, &source))
        .await
        .map_err(|e| format!("导入任务调度失败: {e}"))?
}

#[tauri::command]
pub fn remove_wallpaper(app: AppHandle, path: String) -> Result<(), String> {
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

/// 前端 canvas 截帧生成的 JPEG dataURL，解码后写入封面目录，返回封面路径
#[tauri::command]
pub fn save_thumbnail(app: AppHandle, path: String, data_url: String) -> Result<String, String> {
    let dir = settings::resolve_wallpapers_dir(&app)?;
    let target = PathBuf::from(&path);
    ensure_inside(&dir, &target)?;

    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("文件名异常")?;
    let b64 = data_url
        .strip_prefix(THUMB_DATA_URL_PREFIX)
        .ok_or("封面数据格式异常")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("封面数据解码失败: {e}"))?;

    let thumb = thumb_path_for(&dir, name);
    fs::create_dir_all(dir.join(THUMB_DIR)).map_err(|e| format!("创建封面目录失败: {e}"))?;
    fs::write(&thumb, bytes).map_err(|e| format!("写入封面失败: {e}"))?;
    Ok(thumb.to_string_lossy().into_owned())
}
