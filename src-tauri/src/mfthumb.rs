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

use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFSample, IMFSourceReader, MFCreateMediaType, MFCreateSourceReaderFromURL,
    MFMediaType_Video, MFVideoFormat_NV12, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE,
    MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_PD_DURATION, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantToUInt64, PROPVARIANT};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VT_I8;

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

/// 生成封面到 out_jpg；返回 Err 说明原因（含黑帧重试后仍失败）。
/// attempt 为重新生成的次数（0 = 首次自动生成）：>0 时在候选点位前插入
/// 按时长比例轮换的采样点，让每次「重新生成封面」换一帧——固定点位
/// 恰好撞上坏帧/黑帧时，重试不会永远复现同一帧
pub fn generate(path: &Path, out_jpg: &Path, attempt: u32) -> Result<(), String> {
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
    let result = generate_inner(path, out_jpg, attempt);
    if com_initialized {
        unsafe { CoUninitialize() };
    }
    result
}

fn generate_inner(path: &Path, out_jpg: &Path, attempt: u32) -> Result<(), String> {
    probe::ensure_mf_startup()?;

    // 候选时间点：头部 1 秒 / 中部 / 0.2 秒，逐一尝试直到取到非黑帧。
    // 中点候选先 seek 再读——顺序解码到半程要处理整段前半的帧，长视频上
    // 一次封面生成可能长达分钟级；头部候选不值得 seek（顺序读前几秒更便宜）
    let duration_100ns = read_duration(path);
    let mut candidates: Vec<(i64, bool)> = vec![(1_000_000, false)];
    if let Some(d) = duration_100ns {
        let mid = d / 2;
        if mid > 100_000 {
            candidates.push((mid, true));
        }
    }
    candidates.push((200_000, false));
    // 重新生成：按时长比例轮换的点位插到最前（seek 到位，避免顺序解码烧样本），
    // 原有固定点位保留作兜底（新点位恰好黑帧时继续往后试）
    if attempt > 0 {
        if let Some(d) = duration_100ns {
            const RETRY_FRACS: [f64; 6] = [0.3, 0.6, 0.45, 0.7, 0.35, 0.65];
            let frac = RETRY_FRACS[(attempt - 1) as usize % RETRY_FRACS.len()];
            let t = ((d as f64) * frac) as i64;
            if t > 100_000 {
                candidates.insert(0, (t, true));
            }
        }
    }

    let mut last_err = String::from("未取得任何视频帧");
    for (target, seek) in candidates {
        match unsafe { grab_frame(path, target, seek) } {
            Ok(Some(mut frame)) => {
                if is_black_frame(&frame) {
                    last_err = "候选时间点为黑帧".into();
                    continue;
                }
                // 顶部色度零行回填（部分解码 surface 的常态，不是偶发）：
                // 跳候选会因候选全脏而失败，修复数据才能保证封面必然生成
                repair_uninit_uv_rows(&mut frame);
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
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .ok()?;
        PropVariantToUInt64(&pv).ok().map(|v| v as i64)
    }
}

/// 把读取起点 seek 到 target_100ns。官方文档：时间格式 GUID 传 GUID_NULL
/// 即 100ns 单位，PROPVARIANT 置 VT_I8。seek 不保证精确（落在目标前最近的
/// 关键帧），后续 ReadSample 照常推进到目标时间；失败仅记录、不影响流程
unsafe fn seek_reader(reader: &IMFSourceReader, target_100ns: i64) {
    let mut pv: PROPVARIANT = std::mem::zeroed();
    // 嵌套：PROPVARIANT.0 → ManuallyDrop<PROPVARIANT_0_0> → 匿名 union（hVal = VT_I8 载荷）
    // 中层的 ManuallyDrop 需显式解引用后才能写字段
    let inner = &mut *pv.Anonymous.Anonymous;
    inner.vt = VT_I8;
    inner.Anonymous.hVal = target_100ns;
    if let Err(e) = reader.SetCurrentPosition(&GUID::zeroed(), &pv) {
        if cfg!(debug_assertions) {
            println!("[mfthumb] seek 到 {target_100ns} 失败: {e}");
        }
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

/// 抓取 ≥ target_100ns 的第一帧；None = 流结束前没有到达目标时间的样本。
/// seek = true 时先把读取起点跳到目标时间附近（落在目标前最近的关键帧），
/// 再顺序读到达目标；失败则保持从 0 顺序读，由 MAX_SAMPLES 上限兜底
unsafe fn grab_frame(path: &Path, target_100ns: i64, seek: bool) -> Result<Option<Frame>, String> {
    let reader = new_reader(path)?;
    if seek {
        seek_reader(&reader, target_100ns);
    }
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
    // NV12 标称行跨度（媒体类型）：仅作为连续缓冲回退路径的解读依据；
    // 真实 pitch 由 extract_frame 里的 Lock2D 决定（见其注释），缺失时 16 对齐估算
    let stride: i32 = match cur.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
        Ok(v) => v as i32,
        Err(_) => (width as i32 + 15) & !15,
    }
    .max(width as i32); // 负跨度极罕见（连续缓冲），按宽度下限兜底防越界

    let mut last_frame: Option<Frame> = None;
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
            if let Some(frame) = extract_frame(&sample, width, height, stride) {
                last_frame = Some(frame);
            }
        }
        if take {
            return Ok(last_frame);
        }
    }
    Ok(last_frame)
}

/// 从视频样本提取 NV12 帧。
///
/// 必须优先走 IMF2DBuffer::Lock2D：解码 surface 的真实行距（pitch）与媒体
/// 类型的 DEFAULT_STRIDE 不是一回事。非对齐宽度素材上两者会不一致——
/// 实测 2126x1440 素材：DEFAULT_STRIDE=2126（等于宽度）、连续缓冲长度反推
/// 2128、Lock2D 真实 pitch=2176（64 对齐），三者互不相等。按标称 stride
/// 解读真实 pitch 的 surface 会逐行错位（每行差 50 字节、1440 行累计错开
/// 30+ 行），表现为封面斜向错乱 + 行尾零 padding 拼成的对角绿线。
///
/// Lock2D 不可用或给出负 pitch（极罕见，自底向上布局）时，回退到「连续
/// 缓冲＋媒体类型 stride」的旧路径（对齐宽度素材两者一致，历来工作正常）。
/// 返回的帧统一为紧凑 NV12（stride = width，padding 不带入），后续黑帧
/// 检测 / UV 回填 / JPEG 编码全部按紧帧行距工作，无需再关心 surface 布局
unsafe fn extract_frame(
    sample: &IMFSample,
    width: u32,
    height: u32,
    mt_stride: i32,
) -> Option<Frame> {
    let buffer = match sample.ConvertToContiguousBuffer() {
        Ok(b) => b,
        Err(e) => {
            if cfg!(debug_assertions) {
                println!("[mfthumb] ConvertToContiguousBuffer 失败: {e}");
            }
            return None;
        }
    };
    // 路径 1：2D 接口给出解码 surface 的真实 pitch
    if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut pitch: i32 = 0;
        if b2.Lock2D(&mut ptr as *mut _, &mut pitch).is_ok() {
            if !ptr.is_null() && pitch >= width as i32 {
                let compact = copy_nv12_compact(ptr, pitch as usize, width, height);
                let _ = b2.Unlock2D();
                return Some(Frame {
                    width,
                    height,
                    stride: width as i32,
                    data: compact,
                });
            }
            // 负 pitch / 异常值：解锁后走回退路径
            let _ = b2.Unlock2D();
        }
    }
    // 路径 2（回退）：连续缓冲 + 媒体类型 stride
    let need = (mt_stride as usize * height as usize) * 3 / 2;
    let mut ptr: *mut u8 = std::ptr::null_mut();
    let mut max_len = 0u32;
    let mut cur_len = 0u32;
    if let Err(e) = buffer.Lock(&mut ptr as *mut _, Some(&mut max_len), Some(&mut cur_len)) {
        if cfg!(debug_assertions) {
            println!("[mfthumb] Lock 失败: {e}");
        }
        return None;
    }
    let copied = if ptr.is_null() || cur_len == 0 {
        let _ = buffer.Unlock();
        return None;
    } else {
        // 拷贝出锁定期内的数据，尽快解锁
        let bytes = std::slice::from_raw_parts(ptr, cur_len.min(max_len) as usize).to_vec();
        let _ = buffer.Unlock();
        bytes
    };
    if cfg!(debug_assertions) && copied.len() < need {
        println!(
            "[mfthumb] 帧数据偏小: {} < need {need}（w={width} h={height} stride={mt_stride}）",
            copied.len()
        );
    }
    (copied.len() >= need).then_some(Frame {
        width,
        height,
        stride: mt_stride,
        data: copied,
    })
}

/// 按 pitch 逐行拷贝有效宽度 → 紧凑 NV12（stride = width，无 padding）
unsafe fn copy_nv12_compact(src: *const u8, pitch: usize, width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 3 / 2];
    for y in 0..h {
        std::ptr::copy_nonoverlapping(src.add(y * pitch), out.as_mut_ptr().add(y * w), w);
    }
    // UV 平面紧接 Y 平面之后（NV12 布局），行距与 Y 相同、行数减半
    for y in 0..h / 2 {
        std::ptr::copy_nonoverlapping(
            src.add((h + y) * pitch),
            out.as_mut_ptr().add(w * h + y * w),
            w,
        );
    }
    out
}

/// 回填色度平面顶部的未初始化全零行。
///
/// 部分 MF 解码 surface 的 UV 平面前几行不保证写入（保持分配时的零值，
/// 实测为常态而非偶发）：u=v=0 经 BT.601 转换为 (0, y+91, 0) 的纯绿，
/// 表现为封面顶部一条绿线。修复方式：找到其后第一个非零 UV 行，把有效
/// 宽度内的色度复制回来（色彩连续，顶部 1~2 行近似原色）；stride 尾部
/// 的对齐 padding 不参与。整个色度平面都零的极端情况保持原样（画面全
/// 灰绿也已无救，且真实素材不会出现）
fn repair_uninit_uv_rows(f: &mut Frame) {
    let stride = f.stride as usize;
    let uv_base = stride * f.height as usize;
    if uv_base >= f.data.len() {
        return;
    }
    // UV 交错布局：每 2 个像素共享 1 组 (u,v)，有效字节数恰为 width
    let check_bytes = f.width as usize;
    let uv_rows = (f.data.len() - uv_base) / stride;
    let line = |row: usize| -> Option<std::ops::Range<usize>> {
        let start = uv_base + row * stride;
        (row < uv_rows).then_some(start..start + check_bytes)
    };
    let is_zero =
        |range: &std::ops::Range<usize>| f.data[range.start..range.end].iter().all(|&b| b == 0);
    // 从头数连续零行，第一个非零行即回填源。
    // 零区大小随分辨率/解码 surface 变化（实测 1080p 为 8 行），不设硬编码上限
    let zero_rows = (0..uv_rows)
        .take_while(|&row| line(row).map(|r| is_zero(&r)).unwrap_or(false))
        .count();
    if zero_rows == 0 {
        return; // 首行就有数据，无需修复
    }
    let Some(src) = line(zero_rows) else {
        return; // 整个色度平面皆零，无能为力（真实素材不会出现）
    };
    for row in 0..zero_rows {
        if let Some(dst) = line(row) {
            f.data.copy_within(src.clone(), dst.start);
        }
    }
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
    use windows::core::Interface;
    use windows::Win32::Media::MediaFoundation::IMF2DBuffer;

    /// 真机诊断：非对齐宽度素材的 stride 失配实证（我的视频-79.mp4，2126x1440，
    /// 封面斜向错乱 + 对角绿线）。对比三个数：媒体类型 DEFAULT_STRIDE、
    /// 按连续缓冲区长度反推的 stride、IMF2DBuffer::Lock2D 给出的真实 pitch。
    /// 若三者不一致即证实：按媒体类型 stride 解读解码 surface 是错的
    #[test]
    fn diagnose_stride_mismatch() {
        let path = std::path::PathBuf::from(r"D:\动态壁纸存储\wallpapers\我的视频-79.mp4");
        if !path.exists() {
            println!("[stride-diag] 样本不存在，跳过");
            return;
        }
        let mut com_init = false;
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_ok() {
                com_init = true;
            }
        }
        let _ = probe::ensure_mf_startup();
        unsafe {
            match new_reader(&path) {
                Ok(reader) => {
                    let cur = reader
                        .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                        .unwrap();
                    let size = cur.GetUINT64(&MF_MT_FRAME_SIZE).unwrap();
                    let w = (size >> 32) as u32;
                    let h = (size & 0xFFFF_FFFF) as u32;
                    let mt_stride = match cur.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
                        Ok(v) => format!("{}（媒体类型给出）", v as i32),
                        Err(_) => "缺失（会按 16 对齐估算）".to_string(),
                    };
                    let mut flags = 0u32;
                    let mut ts = 0i64;
                    let mut sample = None;
                    let _ = reader.ReadSample(
                        MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                        0,
                        None,
                        Some(&mut flags),
                        Some(&mut ts),
                        Some(&mut sample),
                    );
                    let Some(s) = sample else {
                        println!("[stride-diag] 首个样本为空");
                        return;
                    };
                    let buf = s.ConvertToContiguousBuffer().unwrap();
                    let mut ptr: *mut u8 = std::ptr::null_mut();
                    let mut max_len = 0u32;
                    let mut cur_len = 0u32;
                    let _ = buf.Lock(&mut ptr as *mut _, Some(&mut max_len), Some(&mut cur_len));
                    let _ = buf.Unlock();
                    let implied = cur_len as u64 / (h as u64 * 3 / 2);
                    println!(
                        "[stride-diag] {w}x{h} | DEFAULT_STRIDE={mt_stride} | cur_len={cur_len}（反推 stride≈{implied}）"
                    );
                    let b2 = buf.cast::<IMF2DBuffer>();
                    match b2 {
                        Ok(b2) => {
                            let mut p2: *mut u8 = std::ptr::null_mut();
                            let mut pitch: i32 = 0;
                            if b2.Lock2D(&mut p2 as *mut _, &mut pitch).is_ok() {
                                println!("[stride-diag] IMF2DBuffer::Lock2D 真实 pitch={pitch}");
                                let _ = b2.Unlock2D();
                            } else {
                                println!("[stride-diag] Lock2D 调用失败");
                            }
                        }
                        Err(_) => println!("[stride-diag] 缓冲区无 IMF2DBuffer 接口"),
                    }
                }
                Err(e) => println!("[stride-diag] 打开失败: {e}"),
            }
        }
        if com_init {
            unsafe { CoUninitialize() };
        }
    }

    use super::*;

    /// 色度零行回填的三个场景：
    /// 首行正常不动 / 首行零被回填为源行 / stride 尾部 padding 不参与
    #[test]
    fn uninit_uv_repair() {
        let (w, h) = (64usize, 32usize);
        let frame = |width: u32, stride: usize, data: Vec<u8>| Frame {
            width,
            height: h as u32,
            stride: stride as i32,
            data,
        };
        // 场景 1：首行有数据，原样不动
        let clean = vec![128u8; w * h * 3 / 2];
        let mut f = frame(w as u32, w, clean.clone());
        repair_uninit_uv_rows(&mut f);
        assert_eq!(f.data, clean);
        // 场景 2：UV 首行清零（u=v=0 → 顶部绿线元凶），回填为第二个非零行
        let mut dirty = vec![128u8; w * h * 3 / 2];
        for (i, b) in dirty[w * h + w..w * h + 2 * w].iter_mut().enumerate() {
            *b = (100 + i % 7) as u8; // 第二行放可辨识的非零模式
        }
        for b in dirty[w * h..w * h + w].iter_mut() {
            *b = 0;
        }
        let mut f = frame(w as u32, w, dirty);
        repair_uninit_uv_rows(&mut f);
        assert_eq!(
            &f.data[w * h..w * h + w],
            &f.data[w * h + w..w * h + 2 * w],
            "零行应被回填为源行的值"
        );
        // 场景 3：stride > width 时，零行的 padding 部分保持原样不被写入
        let stride = 80usize;
        let mut padded = vec![0u8; stride * h * 3 / 2];
        // 有效区域填 128，padding 保持 0
        for row in 0..h / 2 {
            let base = stride * h + row * stride;
            for b in padded[base..base + w].iter_mut() {
                *b = 128;
            }
        }
        // 首行有效区清零制造脏行
        for b in padded[stride * h..stride * h + w].iter_mut() {
            *b = 0;
        }
        let mut f = frame(w as u32, stride, padded);
        repair_uninit_uv_rows(&mut f);
        assert_eq!(
            &f.data[stride * h..stride * h + w],
            &f.data[stride * h + stride..stride * h + stride + w],
            "零行有效宽度应等于源行"
        );
        assert!(
            f.data[stride * h + w..stride * h + stride]
                .iter()
                .all(|&b| b == 0),
            "padding 区域不应被回填触碰"
        );
    }

    /// 真机诊断：打印解码帧色度平面前几行状态（顶部绿线问题的实证）
    #[test]
    fn diagnose_uv_head_rows() {
        // 与 generate() 相同的 COM/MF 前置（诊断直连 grab_frame 必须自带）
        let mut com_init = false;
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_ok() {
                com_init = true;
            }
        }
        let _ = probe::ensure_mf_startup();
        for name in ["温泉四叶草.mp4", "1080p (7).mp4", "1080p.mp4"] {
            let p = std::path::PathBuf::from(r"D:\动态壁纸存储\wallpapers").join(name);
            if !p.exists() {
                println!("[uv-diag] {name}: 跳过（样本不存在）");
                continue;
            }
            unsafe {
                match grab_frame(&p, 1_000_000, false) {
                    Ok(Some(f)) => {
                        let stride = f.stride as usize;
                        let uv_base = stride * f.height as usize;
                        println!(
                            "[uv-diag] {name}: {}x{} stride={} data.len()={}",
                            f.width,
                            f.height,
                            f.stride,
                            f.data.len()
                        );
                        // 逐行统计零区大小：找到第一个非零行，打印零区行数
                        let uv_rows = (f.data.len() - uv_base) / stride;
                        let mut first_nonzero = None;
                        for row in 0..uv_rows {
                            let s = uv_base + row * stride;
                            if !f.data[s..s + f.width as usize].iter().all(|&b| b == 0) {
                                first_nonzero = Some(row);
                                break;
                            }
                        }
                        println!(
                            "[uv-diag]   uv 总行数={uv_rows} 首个非零行={:?}",
                            first_nonzero
                        );
                        if let Some(r) = first_nonzero {
                            let s = uv_base + r * stride;
                            println!(
                                "[uv-diag]   非零行首字节={:?}",
                                &f.data[s..(s + 8).min(f.data.len())]
                            );
                        }
                    }
                    Ok(None) => println!("[uv-diag] {name}: 未取到帧（流提前结束）"),
                    Err(e) => println!("[uv-diag] {name}: 抓帧失败 {e}"),
                }
            }
        }
        if com_init {
            unsafe { CoUninitialize() };
        }
    }

    /// 真机诊断：对问题素材（4K60 High@L5.2）跑完整封面生成，打印每一步。
    /// 1080p.mp4 是「UV 顶部零行」样本：验证回填后封面正常（不再顶部绿线）
    #[test]
    fn generate_thumbnail_on_real_files() {
        for p in [
            r"D:\动态壁纸存储\wallpapers\温泉四叶草.mp4",
            r"D:\动态壁纸存储\wallpapers\1080p (7).mp4",
            r"D:\动态壁纸存储\wallpapers\1080p.mp4",
            r"D:\动态壁纸存储\wallpapers\我的视频-79.mp4",
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
            match generate(&src, &out, 0) {
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
