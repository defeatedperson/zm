//! 视频元数据探测：Media Foundation Source Reader 读取分辨率/帧率/时长。
//! 每次探测在独立线程执行并自初始化 COM，避免污染调用线程的 COM 状态。

use serde::{Deserialize, Serialize};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::Media::MediaFoundation::{
    MFCreateSourceReaderFromURL, MFStartup, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_PD_DURATION,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE, MFSTARTUP_LITE, MF_VERSION,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToUInt64;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// 视频元数据（帧率为精确的分子/分母，如 30000/1001 = 29.97）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoMeta {
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub duration_ms: u64,
}

/// 进程级 MF 初始化（引用计数式，进程生命周期内不卸载）
pub fn ensure_mf_startup() -> Result<(), String> {
    static MF: OnceLock<Result<(), String>> = OnceLock::new();
    MF.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE).map_err(|e| e.to_string()) })
        .clone()
}

/// 探测视频元数据；耗时通常 <50ms，内部切换到独立线程执行
pub fn probe_video(path: &Path) -> Result<VideoMeta, String> {
    let path = path.to_path_buf();
    std::thread::spawn(move || probe_inner(&path))
        .join()
        .map_err(|_| "探测线程崩溃".to_string())?
}

fn probe_inner(path: &PathBuf) -> Result<VideoMeta, String> {
    // COM 初始化：同步 Source Reader 官方建议在 MTA 线程使用（STA 线程不泵消息可能死锁）；
    // 已以其他模式初始化时继续（RPC_E_CHANGED_MODE 不算失败）
    let mut com_initialized = false;
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        if hr.is_ok() {
            com_initialized = true;
        } else if hr != RPC_E_CHANGED_MODE {
            return Err(format!("COM 初始化失败: {hr}"));
        }
    }
    let result = probe_mf(path);
    if com_initialized {
        unsafe { CoUninitialize() };
    }
    result
}

fn probe_mf(path: &Path) -> Result<VideoMeta, String> {
    ensure_mf_startup()?;

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let reader = unsafe {
        MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), None)
            .map_err(|e| format!("打开视频失败: {e}"))?
    };

    unsafe {
        // 原生（压缩）媒体类型上带有分辨率/帧率属性
        let media_type = reader
            .GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0)
            .map_err(|e| format!("读取媒体类型失败: {e}"))?;

        // MF_MT_FRAME_SIZE: 低 32 位 = 宽，高 32 位 = 高
        let size = media_type.GetUINT64(&MF_MT_FRAME_SIZE).map_err(|e| {
            println!("[probe] {} 无分辨率信息: {e}", path.display());
            e.to_string()
        })?;
        let width = (size & 0xFFFF_FFFF) as u32;
        let height = (size >> 32) as u32;

        // MF_MT_FRAME_RATE: 高 32 位 = 分子，低 32 位 = 分母
        let (fps_num, fps_den) = match media_type.GetUINT64(&MF_MT_FRAME_RATE) {
            Ok(rate) if (rate >> 32) != 0 => ((rate >> 32) as u32, (rate & 0xFFFF_FFFF) as u32),
            _ => (0, 0),
        };

        // MF_PD_DURATION: 100ns 单位（挂在媒体源级属性上）
        let duration_ms = reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .ok()
            .and_then(|pv| PropVariantToUInt64(&pv).ok())
            .map(|v| v / 10_000)
            .unwrap_or(0);

        Ok(VideoMeta {
            width,
            height,
            fps_num,
            fps_den,
            duration_ms,
        })
    }
}
