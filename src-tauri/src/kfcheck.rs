//! MP4 关键帧结构检测：识别「片尾无关键帧 / 无关键帧」的异常文件。
//!
//! 背景：Chromium（WebView2）播放 `<video loop>` 时，循环回跳与缓冲恢复都
//! 依赖关键帧作为解码恢复点。经无损剪辑（smart-cut）拼接的素材常在非关键
//! 帧处切割，导致片段尾部长时间没有关键帧——播放数秒后管线停滞、画面冻结
//! （paused=false 但不再出帧，实测；纯净 Chromium 同样复现，非应用问题）。
//!
//! 本模块只解析 moov 元数据（stts 样本时间表 + stss 关键帧表），不读码流、
//! 不解码，开销可忽略。非 MP4 容器（webm 等）或解析失败返回 None，不判风险。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// 需要递归下钻的容器 box
const CONTAINERS: [&[u8; 4]; 5] = [b"moov", b"trak", b"mdia", b"minf", b"stbl"];
/// 单个 box 载荷读取上限（防御异常文件；正常 stts/stss 远小于此值）
const MAX_PAYLOAD: u64 = 16 * 1024 * 1024;
/// 容器嵌套深度上限：正常 MP4 为 moov>trak>mdia>minf>stbl 共 5 层。
/// 损坏/构造的文件每层仅需 8 字节就能堆出上万层嵌套，递归会栈溢出
/// 直接杀死进程（0xC00000FD，实测事故），超限直接放弃判定
const MAX_DEPTH: usize = 16;

struct BoxInfo {
    typ: [u8; 4],
    /// 载荷起始位置（紧跟 box 头/扩展长度之后，即首个子 box 的头）
    body: u64,
    /// box 结束位置（不含）
    end: u64,
}

/// 枚举 [start, end) 内的顶层 box；遇到长度异常立即停止（宁缺勿错）
fn read_boxes(f: &mut File, start: u64, end: u64) -> Vec<BoxInfo> {
    let mut out = Vec::new();
    let mut pos = start;
    while pos + 8 <= end {
        if f.seek(SeekFrom::Start(pos)).is_err() {
            break;
        }
        let mut hdr = [0u8; 8];
        if f.read_exact(&mut hdr).is_err() {
            break;
        }
        let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let typ: [u8; 4] = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let mut body = pos + 8;
        if size == 1 {
            // 64 位扩展长度
            let mut big = [0u8; 8];
            if f.read_exact(&mut big).is_err() {
                break;
            }
            size = u64::from_be_bytes(big);
            body += 8;
        } else if size == 0 {
            size = end - pos;
        }
        let Some(box_end) = pos.checked_add(size) else { break };
        if size < 8 || box_end > end || body > box_end {
            break;
        }
        out.push(BoxInfo { typ, body, end: box_end });
        pos = box_end;
    }
    out
}

/// 深度查找指定类型的 box（递归穿过 CONTAINERS），返回载荷范围。
/// 递归必须从子 box 的头（= 当前 box 的 body）开始枚举——传成当前 box
/// 自己的头会无限自我递归导致栈溢出（0xC00000FD，实测教训）
fn find_boxes(f: &mut File, start: u64, end: u64, want: &[u8; 4]) -> Vec<(u64, u64)> {
    find_boxes_at_depth(f, start, end, want, 0)
}

fn find_boxes_at_depth(
    f: &mut File,
    start: u64,
    end: u64,
    want: &[u8; 4],
    depth: usize,
) -> Vec<(u64, u64)> {
    if depth > MAX_DEPTH {
        return Vec::new();
    }
    let mut out = Vec::new();
    for b in read_boxes(f, start, end) {
        if &b.typ == want {
            out.push((b.body, b.end));
        } else if CONTAINERS.iter().any(|c| **c == b.typ) {
            out.extend(find_boxes_at_depth(f, b.body, b.end, want, depth + 1));
        }
    }
    out
}

/// 读取 box 载荷（超上限返回空，防御异常文件）
fn read_payload(f: &mut File, start: u64, end: u64) -> Vec<u8> {
    if end.saturating_sub(start) > MAX_PAYLOAD {
        return Vec::new();
    }
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let len = (end - start) as usize;
    let mut buf = vec![0u8; len];
    if f.read_exact(&mut buf).is_err() {
        return Vec::new();
    }
    buf
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes([
        b[o],
        b[o + 1],
        b[o + 2],
        b[o + 3],
        b[o + 4],
        b[o + 5],
        b[o + 6],
        b[o + 7],
    ])
}

/// 检测 MP4 关键帧结构。
/// 返回 `Some(原因)` 表示该文件直接播放会冻结、需先经「视频优化」重建编码；
/// `None` 表示正常，或非 MP4 / 无法解析（不参与判定，不阻止使用）。
pub fn check_video(path: &Path) -> Option<String> {
    let file_len = path.metadata().map(|m| m.len()).ok()?;
    let mut f = File::open(path).ok()?;

    // 必须存在 moov 才谈得上分析（webm 等其他容器自然返回 None）
    let moovs = find_boxes(&mut f, 0, file_len, b"moov");
    let (moov_start, moov_end) = *moovs.first()?;

    for (trak_start, trak_end) in find_boxes(&mut f, moov_start, moov_end, b"trak") {
        // 只分析视频轨道
        let hdlr_boxes = find_boxes(&mut f, trak_start, trak_end, b"hdlr");
        let is_video = hdlr_boxes
            .first()
            .map(|(s, e)| {
                let p = read_payload(&mut f, *s, *e);
                p.len() >= 12 && &p[8..12] == b"vide"
            })
            .unwrap_or(false);
        if !is_video {
            continue;
        }

        // 时间刻度与时长（mdhd）
        let mdhd_boxes = find_boxes(&mut f, trak_start, trak_end, b"mdhd");
        let (timescale, duration_units) = mdhd_boxes
            .first()
            .and_then(|(s, e)| {
                let p = read_payload(&mut f, *s, *e);
                if p.len() < 20 {
                    return None;
                }
                if p[0] == 1 && p.len() >= 32 {
                    Some((u32_at(&p, 20), u64_at(&p, 24)))
                } else {
                    Some((u32_at(&p, 12), u32_at(&p, 16) as u64))
                }
            })
            .unwrap_or((0, 0));
        if timescale == 0 {
            continue;
        }

        // stts：样本时间表（(数量, 单帧时长) 游程）
        let stts_boxes = find_boxes(&mut f, trak_start, trak_end, b"stts");
        let Some((stts_s, stts_e)) = stts_boxes.first().copied() else {
            continue;
        };
        let stts = read_payload(&mut f, stts_s, stts_e);
        if stts.len() < 8 {
            continue;
        }
        let runs = u32_at(&stts, 4) as usize;
        if stts.len() < 8 + runs * 8 {
            continue;
        }
        let mut sample_count: u64 = 0;
        for i in 0..runs {
            sample_count += u32_at(&stts, 8 + i * 8) as u64;
        }
        if sample_count == 0 {
            continue; // 空轨道或碎片化 MP4，无法判定
        }

        // stss：关键帧样本号表。按 ISO 规范，stss 缺失 = 全部样本都是关键帧
        let stss_boxes = find_boxes(&mut f, trak_start, trak_end, b"stss");
        let keyframes: Vec<u64> = stss_boxes
            .first()
            .map(|(s, e)| {
                let p = read_payload(&mut f, *s, *e);
                if p.len() < 8 {
                    return Vec::new();
                }
                let n = u32_at(&p, 4) as usize;
                if p.len() < 8 + n * 4 {
                    return Vec::new();
                }
                (0..n).map(|i| u32_at(&p, 8 + i * 4) as u64).collect()
            })
            .unwrap_or_default();
        if stss_boxes.is_empty() {
            continue; // 无 stss：全部样本皆关键帧，安全
        }
        if keyframes.is_empty() {
            return Some("编码结构异常：整条视频没有关键帧".into());
        }

        // 最后一个关键帧的呈现时间 = 位于它之前的所有样本时长之和
        let last_kf = *keyframes.last()?;
        if last_kf == 0 || last_kf > sample_count {
            continue; // 表项异常，不判定
        }
        let mut consumed: u64 = 0;
        let mut last_kf_units: u64 = 0;
        let mut located = false;
        for i in 0..runs {
            let cnt = u32_at(&stts, 8 + i * 8) as u64;
            let delta = u32_at(&stts, 12 + i * 8) as u64;
            if consumed + cnt >= last_kf {
                // 关键帧是本游程中第 (last_kf - consumed) 个样本，
                // 它之前有 (last_kf - consumed - 1) 个样本
                last_kf_units += (last_kf - consumed - 1) * delta;
                located = true;
                break;
            }
            consumed += cnt;
            last_kf_units += cnt * delta;
        }
        if !located {
            continue; // 时间表与关键帧表不一致，不判定
        }

        let duration_ms = duration_units * 1000 / timescale as u64;
        let last_kf_ms = last_kf_units * 1000 / timescale as u64;
        let tail_ms = duration_ms.saturating_sub(last_kf_ms);
        // 判定阈值：片尾无关键帧时长超过 max(3 秒, 时长的 1/4) 即视为异常。
        // 正常编码的关键帧间隔为 1~2 秒，留足了余量，误报面极小
        let threshold = (3_000u64).max(duration_ms / 4);
        if tail_ms > threshold {
            println!(
                "[kfcheck] {}：样本 {}，关键帧 {}（最后 @{}ms），片尾 {}ms 无关键帧 → 判定异常",
                path.display(),
                sample_count,
                keyframes.len(),
                last_kf_ms,
                tail_ms
            );
            return Some(format!(
                "编码结构异常：片尾约 {} 秒没有关键帧",
                (tail_ms as f64 / 1000.0).round() as u64
            ));
        }
        return None; // 视频轨道判定完毕
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 高码率样本（温泉四叶草.mp4，4K60@210Mbps）。其关键帧结构实为正常
    /// （每 0.5s 均匀分布，历史误判已纠正——真正病因是码率，由 probe 层拦截）。
    /// 本测试的核心价值：防止 find_boxes 递归回归（曾因递归参数错误导致
    /// 栈溢出 0xC00000FD，导入即闪退、启动即闪退）
    #[test]
    fn high_bitrate_sample_no_crash() {
        let p = std::path::PathBuf::from(r"D:\动态壁纸工具\测试视频\温泉四叶草.mp4");
        if !p.exists() {
            return; // 非开发机上跳过
        }
        let r = check_video(&p);
        assert_eq!(r, None, "关键帧结构正常，不应误报");
    }

    /// 正常样本（备份目录里的原件，应安全）
    #[test]
    fn normal_file_clean() {
        let dir = std::path::PathBuf::from(
            std::env::var("LOCALAPPDATA")
                .map(|v| format!("{v}\\幻梦桌面\\wallpapers"))
                .unwrap_or_default(),
        );
        let Ok(items) = std::fs::read_dir(&dir) else {
            return; // 目录不存在则跳过
        };
        for item in items.flatten() {
            let p = item.path();
            if p.extension().and_then(|e| e.to_str()) == Some("mp4") {
                assert_eq!(check_video(&p), None, "正常样本不应误报: {}", p.display());
                return;
            }
        }
    }
}
