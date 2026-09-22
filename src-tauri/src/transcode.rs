//! 视频优化转码：MF 解码 →（缩放/降帧）→ H.264 编码。
//! 单任务串行执行，进度事件广播，完成后原件移入备份目录、优化副本原位替换。

use serde::Serialize;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};
use windows::core::PCWSTR;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::{
    MFCreateMediaType, MFCreateSinkWriterFromURL, MFCreateSourceReaderFromURL,
    MFVideoFormat_H264, MFVideoFormat_NV12, MFMediaType_Video, MFVideoInterlace_Progressive,
    IMFMediaType, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_PD_DURATION,
    MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READER_MEDIASOURCE,
};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use crate::probe;
use crate::settings;
use crate::wallpaper;

/// 全局单任务标记（同一时刻只允许一个转码）
static TRANSCODING: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);

const CANCELLED_MSG: &str = "已取消";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscodeProgress {
    path: String,
    percent: u32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TranscodeDone {
    path: String,
    ok: bool,
    cancelled: bool,
    error: Option<String>,
}

/// 应用侧转码参数（目标规格，由前端下拉选择）
struct Plan {
    out_w: u32,
    out_h: u32,
    fps_num: u32,
    fps_den: u32,
    bitrate: u32,
    drop_frames: bool,
}

/// 启动优化转码（后台线程执行，进度事件广播）。
/// target_width/height 传 0 表示保持原始分辨率；fps_num/fps_den 传 0 表示保持原始帧率。
#[tauri::command]
pub fn optimize_start(
    app: AppHandle,
    path: String,
    target_width: u32,
    target_height: u32,
    fps_num: u32,
    fps_den: u32,
) -> Result<(), String> {
    if TRANSCODING.swap(true, Ordering::AcqRel) {
        return Err("已有优化任务进行中，请稍候".into());
    }
    let result = prepare_and_spawn(&app, &path, target_width, target_height, fps_num, fps_den);
    if let Err(e) = result {
        TRANSCODING.store(false, Ordering::Relaxed);
        return Err(e);
    }
    Ok(())
}

fn prepare_and_spawn(
    app: &AppHandle,
    path: &str,
    target_width: u32,
    target_height: u32,
    fps_num: u32,
    fps_den: u32,
) -> Result<(), String> {
    let dir = settings::resolve_wallpapers_dir(app)?;
    let target = PathBuf::from(path);
    crate::gallery::ensure_inside(&dir, &target)?;

    if wallpaper::is_wallpaper_in_use(path) {
        return Err("该视频正在使用中，请先停止对应的屏幕壁纸".into());
    }
    let meta = probe::probe_video(&target)?;

    // 目标分辨率：0 = 原始；偶数对齐；不允许放大
    let mut out_w = if target_width == 0 { meta.width } else { target_width };
    let mut out_h = if target_height == 0 { meta.height } else { target_height };
    out_w -= out_w & 1;
    out_h -= out_h & 1;
    if out_w == 0 || out_h == 0 {
        TRANSCODING.store(false, Ordering::Relaxed);
        return Err("目标分辨率无效".into());
    }
    if out_w > meta.width || out_h > meta.height {
        TRANSCODING.store(false, Ordering::Relaxed);
        return Err("目标分辨率不能超过源视频".into());
    }

    // 目标帧率：0/0 = 保持源帧率；源未知时回退 30fps
    let (src_fps_num, src_fps_den) = (meta.fps_num, meta.fps_den);
    let (fps_num, fps_den) = if fps_num > 0 && fps_den > 0 {
        (fps_num, fps_den)
    } else if src_fps_num > 0 && src_fps_den > 0 {
        (src_fps_num, src_fps_den)
    } else {
        (30, 1)
    };
    let src_fps = if src_fps_den > 0 {
        src_fps_num as f64 / src_fps_den as f64
    } else {
        0.0
    };
    let out_fps = fps_num as f64 / fps_den as f64;
    if out_fps < 12.0 {
        TRANSCODING.store(false, Ordering::Relaxed);
        return Err("目标帧率过低".into());
    }
    let drop_frames = src_fps > 0.0 && (out_fps - src_fps).abs() > 0.5;

    let bitrate =
        ((out_w as f64 * out_h as f64 * out_fps * 0.08) as u32).clamp(1_500_000, 40_000_000);
    let plan = Plan {
        out_w,
        out_h,
        fps_num,
        fps_den,
        bitrate,
        drop_frames,
    };
    let backup_dir = backup_dir(app)?;

    CANCEL.store(false, Ordering::Relaxed);
    let app = app.clone();
    let path = path.to_string();
    std::thread::spawn(move || {
        let result = run_transcode(&app, Path::new(&path), &plan, &backup_dir);
        TRANSCODING.store(false, Ordering::Relaxed);
        let done = match result {
            Ok(()) => TranscodeDone {
                path,
                ok: true,
                cancelled: false,
                error: None,
            },
            Err(e) if e == CANCELLED_MSG => TranscodeDone {
                path,
                ok: false,
                cancelled: true,
                error: None,
            },
            Err(e) => TranscodeDone {
                path,
                ok: false,
                cancelled: false,
                error: Some(e),
            },
        };
        let _ = app.emit("transcode:done", done);
    });
    Ok(())
}

fn run_transcode(
    app: &AppHandle,
    path: &Path,
    plan: &Plan,
    backup_dir: &Path,
) -> Result<(), String> {
    // COM 初始化：与 probe.rs 相同的隔离模式；同步 Source Reader/Sink Writer 建议 MTA
    let mut com_initialized = false;
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_ok() {
            com_initialized = true;
        } else if hr != RPC_E_CHANGED_MODE {
            return Err(format!("COM 初始化失败: {hr}"));
        }
    }
    let result = transcode_inner(app, path, plan, backup_dir);
    if com_initialized {
        unsafe { CoUninitialize() };
    }
    result
}

fn transcode_inner(
    app: &AppHandle,
    path: &Path,
    plan: &Plan,
    backup_dir: &Path,
) -> Result<(), String> {
    probe::ensure_mf_startup()?;

    let temp_out = temp_output_path(path);
    let _ = std::fs::remove_file(&temp_out);

    let transcode = (|| -> Result<(), String> {
        unsafe {
            let reader = create_source_reader(path)?;
            let decoded_type = MFCreateMediaType().map_err(|e| format!("创建媒体类型失败: {e}"))?;
            decoded_type
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| e.to_string())?;
            decoded_type
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
                .map_err(|e| e.to_string())?;
            reader
                .SetCurrentMediaType(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    None,
                    &decoded_type,
                )
                .map_err(|e| format!("配置解码输出失败: {e}"))?;
            let input_type = reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                .map_err(|e| format!("获取解码类型失败: {e}"))?;

            let duration_100ns = reader
                .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
                .ok()
                .and_then(|pv| windows::Win32::System::Com::StructuredStorage::PropVariantToUInt64(&pv).ok())
                .unwrap_or(0);

            let output_type = build_output_type(plan)?;
            let sink = MFCreateSinkWriterFromURL(PCWSTR(wide(&temp_out).as_ptr()), None, None)
                .map_err(|e| format!("创建编码器失败: {e}"))?;

            let stream_index = sink
                .AddStream(&output_type)
                .map_err(|e| format!("配置输出流失败: {e}"))?;
            sink.SetInputMediaType(stream_index, &input_type, None)
                .map_err(|e| format!("配置输入失败（不支持的规格）: {e}"))?;
            sink.BeginWriting()
                .map_err(|e| format!("启动编码失败: {e}"))?;

            let frame_interval: i64 = if plan.drop_frames {
                (10_000_000.0 / (plan.fps_num as f64 / plan.fps_den as f64)) as i64
            } else {
                0
            };
            let mut next_accept: i64 = 0;
            let mut last_emit = Instant::now();
            let mut written: u32 = 0;

            loop {
                if CANCEL.load(Ordering::Relaxed) {
                    return Err(CANCELLED_MSG.into());
                }
                let mut flags: u32 = 0;
                let mut ts: i64 = 0;
                let mut sample = None;
                reader
                    .ReadSample(
                        MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                        0,
                        None,
                        Some(&mut flags),
                        Some(&mut ts),
                        Some(&mut sample),
                    )
                    .map_err(|e| format!("读取视频帧失败: {e}"))?;
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    break;
                }
                let Some(sample) = sample else { continue };

                let t = sample.GetSampleTime().unwrap_or(ts);
                if frame_interval > 0 {
                    if t < next_accept {
                        continue;
                    }
                    next_accept = t + frame_interval;
                }

                sink.WriteSample(stream_index, &sample)
                    .map_err(|e| format!("写入视频帧失败: {e}"))?;
                written += 1;

                if duration_100ns > 0 && last_emit.elapsed() >= Duration::from_millis(250) {
                    let percent =
                        ((t as f64 / duration_100ns as f64) * 100.0).clamp(0.0, 99.0) as u32;
                    let _ = app.emit(
                        "transcode:progress",
                        TranscodeProgress {
                            path: path.to_string_lossy().into_owned(),
                            percent,
                        },
                    );
                    last_emit = Instant::now();
                }
            }

            sink.Finalize()
                .map_err(|e| format!("完成编码失败: {e}"))?;
            println!("[transcode] 编码完成，共 {written} 帧");
        }
        Ok(())
    })();

    if let Err(e) = transcode {
        let _ = std::fs::remove_file(&temp_out);
        return Err(e);
    }

    // 成功：原件移入备份目录（重名自动追加序号），优化副本原位替换
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Err("文件名异常".into());
    };
    let backup_target = dedupe_backup_path(backup_dir, name);
    std::fs::rename(path, &backup_target).map_err(|e| format!("移动原件失败: {e}"))?;
    if let Err(e) = std::fs::rename(&temp_out, path) {
        // 极端情况：把原件放回去，保证库里始终有文件
        let _ = std::fs::rename(&backup_target, path);
        return Err(format!("替换文件失败: {e}"));
    }

    // 旧封面/旧元数据已失效：清理并按新文件重建元数据（前端刷新后徽标/封面同步更新）
    if let Some(dir) = path.parent() {
        crate::gallery::invalidate_entry_caches(dir, name);
        match probe::probe_video(path) {
            Ok(m) => {
                if let Err(e) = crate::gallery::write_meta(dir, name, &m) {
                    println!("[transcode] 写入新元数据失败: {e}");
                }
            }
            Err(e) => println!("[transcode] 重新探测失败: {e}"),
        }
    }

    println!(
        "[transcode] 完成：{}x{} @{}fps，原件已备份至 {}",
        plan.out_w,
        plan.out_h,
        plan.fps_num as f64 / plan.fps_den as f64,
        backup_target.display()
    );
    Ok(())
}

// ---------- MF 辅助 ----------

fn create_source_reader(path: &Path) -> Result<windows::Win32::Media::MediaFoundation::IMFSourceReader, String> {
    let wide = wide(path);
    unsafe {
        MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), None)
            .map_err(|e| format!("打开视频失败: {e}"))
    }
}

fn build_output_type(plan: &Plan) -> Result<IMFMediaType, String> {
    unsafe {
        let t = MFCreateMediaType().map_err(|e| format!("创建媒体类型失败: {e}"))?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| e.to_string())?;
        t.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
            .map_err(|e| e.to_string())?;
        t.SetUINT64(
            &MF_MT_FRAME_SIZE,
            ((plan.out_h as u64) << 32) | plan.out_w as u64,
        )
        .map_err(|e| e.to_string())?;
        t.SetUINT64(
            &MF_MT_FRAME_RATE,
            ((plan.fps_num as u64) << 32) | plan.fps_den as u64,
        )
        .map_err(|e| e.to_string())?;
        t.SetUINT32(&MF_MT_AVG_BITRATE, plan.bitrate).map_err(|e| e.to_string())?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(|e| e.to_string())?;
        t.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1).map_err(|e| e.to_string())?;
        Ok(t)
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

// ---------- 文件与目录 ----------

fn temp_output_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("video");
    path.with_file_name(format!("{stem}.tmp.mp4"))
}

/// 备份目录：app_local_data_dir/backup
pub fn backup_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| e.to_string())?
        .join("backup");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建备份目录失败: {e}"))?;
    Ok(dir)
}

fn dedupe_backup_path(backup_dir: &Path, name: &str) -> PathBuf {
    let candidate = backup_dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("video");
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    for i in 2..1000 {
        let candidate = backup_dir.join(format!("{stem} ({i}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    backup_dir.join(format!("{stem}-backup{ext}"))
}

/// 查询备份目录路径（设置页「打开备份目录」用）
#[tauri::command]
pub fn get_backup_dir(app: AppHandle) -> Result<String, String> {
    let dir = backup_dir(&app)?;
    Ok(dir.to_string_lossy().into_owned())
}

/// 取消当前转码
#[tauri::command]
pub fn optimize_cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}
