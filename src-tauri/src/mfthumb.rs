//! 后端封面生成：Media Foundation 解码目标时间点的帧 → NV12→RGB 缩放
//! → 黑帧检测 → JPEG 落盘。
//!
//! 为什么不用前端 `<video>` + canvas：截帧依赖 Chromium 解码，而 WebView2 对
//! 4K60 High@L5.2 的 H.264 会退化为软解，seek 一帧在超时窗口内完不成
//! （实测 200Mbps 素材表现为永远「生成封面中」或解出黑帧）。MF 管路
//! （probe/transcode 同款）解码同样文件毫无压力——转码能整段跑完，抓一帧
//! 更是秒级。生成期间持有媒体文件锁，与转码的「备份+替换」互斥。

use std::fs::File;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::{
    MFCreateMediaType, MFCreateSourceReaderFromURL, MFMediaType_Video, MFVideoFormat_NV12,
    IMFSourceReader, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_PD_DURATION, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToUInt64;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

use crate::probe;

/// 单次抓取最多读取的样本数（60fps 下约 10 秒），防御异常流
const MAX_SAMPLES: u32 = 600;
/// 输出封面最大宽度（与前端旧逻辑一致）
const OUT_WIDTH: u32 = 480;
/// 黑帧判定：Y 平面采样的最大亮度低于该值视为空白帧
const BLACK_LUMA: u8 = 24;

struct Frame {
    width: u32,
    height: u32,
    stride: i32,
    /// NV12：Y 平面 (stride*height) + 交错 UV 平面 (stride*height/2)
    data: Vec<u8>,
}

/// 生成封面到 out_jpg；返回 Err 说明原因（含黑帧重试后仍失败）
pub fn generate(path: &Path, out_jpg: &Path) -> Result<(), String> {
    // COM 初始化：与 probe.rs 相同的隔离模式
    let mut com_initialized = false;
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_ok() {
            com_initialized = true;
        } else if hr != RPC_E_CHANGED_MODE {
            return Err(format!("COM 初始化失败: {hr}"));
        }
    }
    let result = generate_inner(path, out_jpg);
    if com_initialized {
        unsafe { CoUninitialize() };
    }
    result
}

fn generate_inner(path: &Path, out_jpg: &Path) -> Result<(), String> {
    probe::ensure_mf_startup()?;

    // 候选时间点：头部 1 秒 / 中部 / 0.2 秒，逐一尝试直到取到非黑帧
    let duration_100ns = read_duration(path);
    let mut candidates: Vec<i64> = vec![1_000_000];
    if let Some(d) = duration_100ns {
        let mid = d / 2;
        if mid > 100_000 {
            candidates.push(mid);
        }
    }
    candidates.push(200_000);

    let mut last_err = String::from("未取得任何视频帧");
    for target in candidates {
        match unsafe { grab_frame(path, target) } {
            Ok(Some(frame)) => {
                if is_black_frame(&frame) {
                    last_err = "候选时间点为黑帧".into();
                    continue;
                }
                return encode_jpeg(&frame, out_jpg);
            }
            Ok(None) => {
                last_err = "视频过短，未读取到目标时间点的帧".into();
            }
            Err(e) => last_err = e,
        }
    }
    Err(format!("无法获取有效画面（{last_err}）"))
}

/// 读取媒体源总时长（100ns）；失败返回 None（不影响候选点兜底）
fn read_duration(path: &Path) -> Option<i64> {
    unsafe {
        let reader = new_reader(path).ok()?;
        let pv = reader
            .GetPresentationAttribute(
                MF_SOURCE_READER_MEDIASOURCE.0 as u32,
                &MF_PD_DURATION,
            )
            .ok()?;
        PropVariantToUInt64(&pv).ok().map(|v| v as i64)
    }
}

unsafe fn new_reader(path: &Path) -> Result<IMFSourceReader, String> {
    let wide = wide(path);
    let reader = MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), None)
        .map_err(|e| format!("打开视频失败: {e}"))?;
    // 输出固定为 NV12：MF 内部自带解码器/转换器，尺寸信息保留原视频
    let mt = MFCreateMediaType().map_err(|e| format!("创建媒体类型失败: {e}"))?;
    mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
        .map_err(|e| e.to_string())?;
    mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
        .map_err(|e| e.to_string())?;
    reader
        .SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &mt)
        .map_err(|e| format!("配置解码输出失败: {e}"))?;
    Ok(reader)
}

/// 抓取 ≥ target_100ns 的第一帧；None = 流结束前没有到达目标时间的样本
unsafe fn grab_frame(path: &Path, target_100ns: i64) -> Result<Option<Frame>, String> {
    let reader = new_reader(path)?;
    let cur = reader
        .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
        .map_err(|e| format!("获取解码类型失败: {e}"))?;
    let size = cur
        .GetUINT64(&MF_MT_FRAME_SIZE)
        .map_err(|e| format!("读取分辨率失败: {e}"))?;
    // MF_MT_FRAME_SIZE 打包：高 32 位 = 宽，低 32 位 = 高（官方文档；
    // 实测 4K 横屏素材低 32 位为 2160，若按"低=宽"会颠倒宽高）
    let width = (size >> 32) as u32;
    let height = (size & 0xFFFF_FFFF) as u32;
    if width == 0 || height == 0 {
        return Err("分辨率异常".into());
    }
    // NV12 行跨度：优先取系统给出值，缺失时按 16 对齐估算
    let stride: i32 = match cur.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
        Ok(v) => v as i32,
        Err(_) => (width as i32 + 15) & !15,
    }
    .max(width as i32); // 负跨度极罕见（连续缓冲），按宽度下限兜底防越界

    let mut last_frame: Option<Frame> = None;
    let need = (stride as usize * height as usize) * 3 / 2;
    for i in 0..MAX_SAMPLES {
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
        if cfg!(debug_assertions) {
            println!(
                "[mfthumb] sample#{i} flags={flags:#x} ts={ts} present={} target={target_100ns}",
                sample.is_some()
            );
        }
        if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            break;
        }
        let Some(sample) = sample else { continue };
        let t = sample.GetSampleTime().unwrap_or(ts);

        // 达到目标时间：取本帧返回；未达到则记录最近一帧（短视频兜底）
        let take = t >= target_100ns;
        if take || last_frame.is_none() {
            let buffer = match sample.ConvertToContiguousBuffer() {
                Ok(b) => b,
                Err(e) => {
                    if cfg!(debug_assertions) {
                        println!("[mfthumb] ConvertToContiguousBuffer 失败: {e}");
                    }
                    continue;
                }
            };
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;
            if let Err(e) =
                buffer.Lock(&mut ptr as *mut _, Some(&mut max_len), Some(&mut cur_len))
            {
                if cfg!(debug_assertions) {
                    println!("[mfthumb] Lock 失败: {e}");
                }
                continue;
            }
            let copied = if ptr.is_null() || cur_len == 0 {
                let _ = buffer.Unlock();
                continue;
            } else {
                // 拷贝出锁定期内的数据，尽快解锁
                let bytes =
                    std::slice::from_raw_parts(ptr, cur_len.min(max_len) as usize).to_vec();
                let _ = buffer.Unlock();
                bytes
            };
            if cfg!(debug_assertions) && copied.len() < need {
                println!(
                    "[mfthumb] 帧数据偏小: {} < need {need}（w={width} h={height} stride={stride}）",
                    copied.len()
                );
            }
            if copied.len() >= need {
                last_frame = Some(Frame {
                    width,
                    height,
                    stride,
                    data: copied,
                });
            }
        }
        if take {
            return Ok(last_frame);
        }
    }
    Ok(last_frame)
}

/// Y 平面采样检测黑帧（与前端旧逻辑同阈值）
fn is_black_frame(f: &Frame) -> bool {
    let mut max_luma: u8 = 0;
    let stride = f.stride as usize;
    let y_plane = &f.data[..stride * f.height as usize];
    let step = (y_plane.len() / 4096).max(1);
    for &y in y_plane.iter().step_by(step) {
        if y > max_luma {
            max_luma = y;
            if max_luma > BLACK_LUMA {
                return false;
            }
        }
    }
    true
}

/// NV12 → RGB 缩放（整数倍抽样）→ JPEG 落盘
fn encode_jpeg(f: &Frame, out_jpg: &Path) -> Result<(), String> {
    let step = (f.width as f64 / OUT_WIDTH as f64).ceil().max(1.0) as u32;
    let out_w = (f.width / step).max(1);
    let out_h = (f.height / step).max(1);
    let stride = f.stride as usize;
    let h = f.height as usize;
    let uv_base = stride * h;

    let mut rgb = Vec::with_capacity((out_w * out_h * 3) as usize);
    for oy in 0..out_h as usize {
        let sy = oy * step as usize;
        let y_row = &f.data[sy * stride..];
        let uv_row = &f.data[uv_base + (sy / 2) * stride..];
        for ox in 0..out_w as usize {
            let sx = ox * step as usize;
            let y = y_row[sx] as i32;
            let u = uv_row[sx & !1] as i32 - 128;
            let v = uv_row[(sx & !1) + 1] as i32 - 128;
            // BT.601 有限范围简化为全范围（封面用途足够）
            let r = (y + ((91881 * v) >> 16)).clamp(0, 255) as u8;
            let g = (y - ((22554 * u + 46802 * v) >> 16)).clamp(0, 255) as u8;
            let b = (y + ((116130 * u) >> 16)).clamp(0, 255) as u8;
            rgb.push(r);
            rgb.push(g);
            rgb.push(b);
        }
    }

    let mut file = File::create(out_jpg).map_err(|e| format!("创建封面文件失败: {e}"))?;
    let enc = jpeg_encoder::Encoder::new(&mut file, 82);
    enc.encode(
        &rgb,
        out_w as u16,
        out_h as u16,
        jpeg_encoder::ColorType::Rgb,
    )
    .map_err(|e| format!("JPEG 编码失败: {e}"))?;
    file.flush().map_err(|e| format!("写入封面失败: {e}"))?;
    Ok(())
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机诊断：对问题素材（4K60 High@L5.2）跑完整封面生成，打印每一步
    #[test]
    fn generate_thumbnail_on_real_files() {
        for p in [
            r"D:\动态壁纸存储\wallpapers\温泉四叶草.mp4",
            r"D:\动态壁纸存储\wallpapers\1080p (7).mp4",
        ] {
            let src = std::path::PathBuf::from(p);
            if !src.exists() {
                println!("[mfthumb-test] 跳过（不存在）: {p}");
                continue;
            }
            let out = std::env::temp_dir().join(format!(
                "mfthumb-test-{}.jpg",
                src.file_stem().unwrap_or_default().to_string_lossy()
            ));
            match generate(&src, &out) {
                Ok(()) => {
                    let len = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                    println!("[mfthumb-test] OK {p} => {} ({} 字节)", out.display(), len);
                    assert!(len > 1000, "JPEG 过小，疑似黑帧/坏帧");
                }
                Err(e) => panic!("[mfthumb-test] FAIL {p}: {e}"),
            }
        }
    }
}
