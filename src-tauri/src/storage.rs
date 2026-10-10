//! 存储位置迁移：把 wallpapers + backup 两个目录整体搬到用户指定的根目录下。
//!
//! 流程：前置校验（壁纸已关闭、无导入/转码进行中、目标为本地固定磁盘、不与当前位置
//! 嵌套、目标子目录为空、剩余空间足够）→ 全量复制（不动原件）→ 校验（文件数+字节数）
//! → 原子切换配置 → 尽力清理旧目录。任一步失败都不会改动原目录，
//! 数据安全由「先复制后切换」的顺序保证。

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW};
use windows::Win32::System::WindowsProgramming::DRIVE_FIXED;

use crate::gallery;
use crate::settings;
use crate::transcode;
use crate::wallpaper;

/// 迁移进行中标记：导入/删除/转码/应用壁纸等写操作在此期间直接拒绝，
/// 避免复制过程中目录内容变化导致校验失败、或新数据落进旧目录后被清理。
static MIGRATING: AtomicBool = AtomicBool::new(false);

/// 当前是否有迁移任务进行中（其他模块的写操作前置检查用）
pub fn is_migrating() -> bool {
    MIGRATING.load(Ordering::Relaxed)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationResult {
    /// 旧目录清理失败的原因（迁移本身已成功，配置已切换）
    pub warning: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MigrateProgress {
    /// 已复制字节数
    copied: u64,
    /// 总字节数
    total: u64,
}

/// 更改存储位置（整体迁移 wallpapers + backup，需先关闭所有壁纸）。
/// 复制耗时可能较长，放阻塞线程执行，进度以 100ms 节流广播。
#[tauri::command]
pub async fn migrate_storage(
    app: AppHandle,
    new_root: String,
) -> Result<MigrationResult, String> {
    tauri::async_runtime::spawn_blocking(move || migrate_blocking(&app, &new_root))
        .await
        .map_err(|e| format!("迁移任务调度失败: {e}"))?
}

fn migrate_blocking(app: &AppHandle, new_root: &str) -> Result<MigrationResult, String> {
    if MIGRATING.swap(true, Ordering::AcqRel) {
        return Err("已有迁移任务进行中".into());
    }
    let result = migrate_inner(app, new_root);
    MIGRATING.store(false, Ordering::Relaxed);
    result
}

fn migrate_inner(app: &AppHandle, new_root: &str) -> Result<MigrationResult, String> {
    // —— 目标目录规范化与磁盘校验 ——
    let raw = PathBuf::from(new_root);
    if !raw.is_dir() {
        return Err("目标位置不是有效目录".into());
    }
    let new_root = normalize(&raw);
    ensure_local_fixed_drive(&new_root)?;

    // —— 前置状态检查：壁纸已关闭、无导入/转码进行中 ——
    if wallpaper::any_active(app) {
        return Err("请先关闭所有屏幕的动态壁纸，再更改存储位置".into());
    }
    if transcode::is_transcoding() {
        return Err("正在优化视频，请等待完成后再更改存储位置".into());
    }
    if gallery::is_importing() {
        return Err("正在导入素材，请等待完成后再更改存储位置".into());
    }
    wait_backfill_done()?;

    // —— 与当前位置的关系校验 ——
    let old_wallpapers = settings::resolve_wallpapers_dir(app)?;
    let old_backup = settings::resolve_backup_dir(app)?;
    let new_wallpapers = new_root.join("wallpapers");
    let new_backup = new_root.join("backup");

    if same_path(&new_wallpapers, &old_wallpapers) || same_path(&new_backup, &old_backup) {
        return Err("目标位置与当前存储位置相同".into());
    }
    // 互相嵌套会造成递归复制或复制进自身，直接拒绝
    for old in [&old_wallpapers, &old_backup] {
        if path_contains(&new_root, old) || path_contains(old, &new_root) {
            return Err("目标位置不能与当前存储目录互相嵌套".into());
        }
    }
    // 目标子目录必须不存在或为空，避免与已有内容混在一起
    for d in [&new_wallpapers, &new_backup] {
        if !dir_missing_or_empty(d) {
            let name = d.file_name().and_then(|n| n.to_str()).unwrap_or("目标");
            return Err(format!(
                "目标位置下已存在非空的 {name} 文件夹，请先清空该文件夹或换个位置"
            ));
        }
    }

    // —— 统计源数据量并检查目标磁盘剩余空间 ——
    let (wp_files, wp_bytes) = walk_stats(&old_wallpapers);
    let backup_exists = old_backup.exists();
    let (bak_files, bak_bytes) = if backup_exists {
        walk_stats(&old_backup)
    } else {
        (0, 0)
    };
    let total_files = wp_files + bak_files;
    let total_bytes = wp_bytes + bak_bytes;
    ensure_free_space(&new_root, total_bytes)?;

    // —— 全量复制（不动原件，失败即清理半成品并中止） ——
    let _ = app.emit(
        "migrate:progress",
        MigrateProgress {
            copied: 0,
            total: total_bytes,
        },
    );
    let mut ctx = CopyCtx {
        app,
        copied: 0,
        total: total_bytes,
        last_emit: Instant::now(),
    };
    let copied = (|| -> Result<(), String> {
        copy_tree(&mut ctx, &old_wallpapers, &new_wallpapers)?;
        if backup_exists {
            copy_tree(&mut ctx, &old_backup, &new_backup)?;
        }
        Ok(())
    })();
    if let Err(e) = copied {
        let _ = fs::remove_dir_all(&new_wallpapers);
        let _ = fs::remove_dir_all(&new_backup);
        return Err(format!("复制失败：{e}。已中止，原目录未做任何改动"));
    }

    // —— 校验：文件数与总字节数必须与源一致 ——
    let verify = (|| -> Result<(), String> {
        let (nf, nb) = walk_stats(&new_wallpapers);
        if (nf, nb) != (wp_files, wp_bytes) {
            return Err("壁纸目录校验失败".into());
        }
        if backup_exists {
            let (nf, nb) = walk_stats(&new_backup);
            if (nf, nb) != (bak_files, bak_bytes) {
                return Err("备份目录校验失败".into());
            }
        }
        Ok(())
    })();
    if let Err(e) = verify {
        let _ = fs::remove_dir_all(&new_wallpapers);
        let _ = fs::remove_dir_all(&new_backup);
        return Err(format!("复制结果与原目录不一致（{e}）。已中止，原目录未做任何改动"));
    }

    // —— 切换配置（原子写 settings.json）并放开新位置的访问范围 ——
    settings::apply_storage_root(
        app,
        Some(new_root.to_string_lossy().into_owned()),
        &old_wallpapers,
    )?;
    extend_runtime_scope(app);
    // 内存中的「最近壁纸」指向旧路径，已失效
    wallpaper::clear_last_wallpaper();

    // —— 尽力清理旧目录（被占用等导致失败不影响迁移结果，交给设置页的手动清理入口） ——
    let mut warning = None;
    for old in [&old_wallpapers, &old_backup] {
        if old.exists() {
            if let Err(e) = fs::remove_dir_all(old) {
                warning = Some(format!(
                    "旧目录 {} 删除失败（{e}），可稍后在设置页手动清理",
                    old.display()
                ));
            }
        }
    }
    println!(
        "[storage] 迁移完成：{total_files} 个文件 / {:.2} GB → {}",
        total_bytes as f64 / 1e9,
        new_root.display()
    );
    Ok(MigrationResult { warning })
}

/// 一键清理默认存储位置的 wallpapers 与 backup 残留。
/// 仅在自定义位置「实际生效」时允许——若自定义位置的磁盘未接入（已回退默认），
/// 默认目录正是当前正在使用的数据，必须拒绝。
#[tauri::command]
pub fn delete_default_dirs(app: AppHandle) -> Result<(), String> {
    if !settings::using_custom_root(&app) {
        return Err(
            "当前实际使用的是默认存储位置（未配置自定义位置，或其磁盘未接入），不能清理".into(),
        );
    }
    if MIGRATING.load(Ordering::Relaxed) {
        return Err("正在迁移存储位置，请稍候".into());
    }
    let base = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    for name in ["wallpapers", "backup"] {
        let dir = base.join(name);
        if dir.exists() {
            if let Err(e) = fs::remove_dir_all(&dir) {
                errors.push(format!("{}：{e}", dir.display()));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("部分目录删除失败：{}", errors.join("；")))
    }
}

/// 把自定义存储根目录加入 asset 协议运行时白名单（前端 convertFileSrc 才能加载
/// 新位置的视频与封面）。启动时与迁移切换后各调用一次。
pub fn extend_runtime_scope(app: &AppHandle) {
    let Some(root) = settings::load(app).storage_root else {
        return;
    };
    let path = PathBuf::from(&root);
    if !path.is_dir() {
        return;
    }
    if let Err(e) = app.asset_protocol_scope().allow_directory(&path, true) {
        println!("[storage] 扩展 asset 访问范围失败: {e}");
    }
}

// ---------- 辅助 ----------

/// 等待启动时的元数据回填结束（短时任务，超时则让用户稍后重试）
fn wait_backfill_done() -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while gallery::is_backfilling() {
        if Instant::now() >= deadline {
            return Err("系统正在后台处理视频信息，请稍后再试".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// 去掉 Windows canonicalize 产生的 \\?\ 前缀，得到便于展示与比较的普通路径
fn normalize(path: &Path) -> PathBuf {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let s = canon.to_string_lossy();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}

/// Windows 路径大小写不敏感，统一小写比较
fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_ascii_lowercase() == b.to_string_lossy().to_ascii_lowercase()
}

/// child 是否位于 parent 内（含相等）
fn path_contains(parent: &Path, child: &Path) -> bool {
    let p = parent.to_string_lossy().to_ascii_lowercase();
    let c = child.to_string_lossy().to_ascii_lowercase();
    c == p || c.starts_with(&format!("{p}\\"))
}

fn dir_missing_or_empty(dir: &Path) -> bool {
    !dir.exists()
        || fs::read_dir(dir)
            .map(|mut it| it.next().is_none())
            .unwrap_or(false)
}

/// 递归统计目录内文件数与总字节数
fn walk_stats(dir: &Path) -> (u64, u64) {
    fn visit(dir: &Path, files: &mut u64, bytes: &mut u64) {
        let Ok(items) = fs::read_dir(dir) else {
            return;
        };
        for item in items.flatten() {
            let path = item.path();
            if path.is_dir() {
                visit(&path, files, bytes);
            } else if path.is_file() {
                *files += 1;
                *bytes += item.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    let (mut files, mut bytes) = (0u64, 0u64);
    visit(dir, &mut files, &mut bytes);
    (files, bytes)
}

struct CopyCtx<'a> {
    app: &'a AppHandle,
    copied: u64,
    total: u64,
    last_emit: Instant,
}

/// 递归复制目录树（含空目录），进度按 100ms 节流广播
fn copy_tree(ctx: &mut CopyCtx, src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| format!("创建目录 {} 失败: {e}", dst.display()))?;
    let items = fs::read_dir(src).map_err(|e| format!("读取目录 {} 失败: {e}", src.display()))?;
    for item in items.flatten() {
        let path = item.path();
        let target = dst.join(item.file_name());
        if path.is_dir() {
            copy_tree(ctx, &path, &target)?;
        } else {
            let len = item.metadata().map(|m| m.len()).unwrap_or(0);
            fs::copy(&path, &target).map_err(|e| {
                format!("复制 {} 失败: {e}", path.display())
            })?;
            ctx.copied += len;
            if ctx.last_emit.elapsed() >= Duration::from_millis(100) {
                let _ = ctx.app.emit(
                    "migrate:progress",
                    MigrateProgress {
                        copied: ctx.copied,
                        total: ctx.total,
                    },
                );
                ctx.last_emit = Instant::now();
            }
        }
    }
    Ok(())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 取路径所在磁盘根（如 D:\）
fn drive_root(path: &Path) -> Result<String, String> {
    path.ancestors()
        .last()
        .map(|r| r.to_string_lossy().into_owned())
        .ok_or_else(|| "无法解析目标位置所在磁盘".to_string())
}

/// 目标必须位于本地固定磁盘：拒绝 U 盘（可移动）、网络盘、光驱等——
/// 移动介质拔出或网络断开后，启动恢复会静默丢失壁纸
fn ensure_local_fixed_drive(path: &Path) -> Result<(), String> {
    let root = drive_root(path)?;
    let ty = unsafe { GetDriveTypeW(PCWSTR(wide(&root).as_ptr())) };
    if ty != DRIVE_FIXED {
        return Err(
            "目标位置不在本地固定磁盘上（U 盘/网络盘等拔出或断开后壁纸将无法使用），请选择本地磁盘"
                .into(),
        );
    }
    Ok(())
}

/// 复制需要临时占用双倍空间，提前检查目标磁盘剩余空间（留 128MB 余量）
fn ensure_free_space(path: &Path, need_bytes: u64) -> Result<(), String> {
    let root = drive_root(path)?;
    let mut available: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide(&root).as_ptr()),
            Some(&mut available as *mut u64),
            None,
            None,
        )
    };
    ok.map_err(|e| format!("查询磁盘剩余空间失败: {e}"))?;
    let need = need_bytes + 128 * 1024 * 1024;
    if available < need {
        return Err(format!(
            "目标磁盘剩余空间不足：需要约 {:.2} GB（含安全余量），当前可用约 {:.2} GB",
            need as f64 / 1e9,
            available as f64 / 1e9
        ));
    }
    Ok(())
}
