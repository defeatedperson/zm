//! 全屏应用检测：某块屏幕被全屏应用占据时暂停该屏壁纸（视频停在当前帧），
//! 退出全屏后恢复。每 2 秒轮询一次，仅在状态变化时向对应窗口发事件。
//! 同一轮枚举顺带喂给 wallpaper::handle_display_change（显示器热插拔/
//! 布局变化监视），不为此另起线程。

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tauri::{AppHandle, Emitter, Monitor};
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_NOT_PRESENT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowLongPtrW, GetWindowRect, IsWindowVisible, GWL_EXSTYLE,
    WS_EX_TOOLWINDOW,
};

use crate::settings;
use crate::wallpaper;

const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// 每块屏当前的暂停状态，只在变化时发事件
fn pause_state() -> &'static Mutex<HashMap<String, bool>> {
    static STATE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 启动后台轮询线程
pub fn start_poller(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(POLL_INTERVAL);
        if let Err(e) = tick(&app) {
            println!("[fullscreen] tick 失败: {e}");
        }
    });
}

/// 查询指定屏幕当前是否处于暂停状态
pub fn is_paused(label: &str) -> bool {
    pause_state()
        .lock()
        .map(|state| state.get(label).copied().unwrap_or(false))
        .unwrap_or(false)
}

fn tick(app: &AppHandle) -> Result<(), String> {
    let monitors = list_monitors(app)?;

    let result = if settings::fullscreen_pause_cached() && wallpaper::has_wallpaper_windows(app) {
        tick_pause(app, &monitors)
    } else {
        // 功能关闭或当前没有任何壁纸窗口：跳过 EnumWindows 全屏枚举空转，
        // 只确保全部恢复（无窗口时暂停状态表为空，resume_all 天然空操作）
        resume_all(app, &monitors)
    };

    // 顺带喂给显示器监视器（复用本轮枚举，内部自带稳定判定与节流）
    wallpaper::handle_display_change(app, &monitors);
    result
}

fn tick_pause(app: &AppHandle, monitors: &[Monitor]) -> Result<(), String> {
    let quns = unsafe { SHQueryUserNotificationState() }.map_err(|e| e.to_string())?;
    // QUNS_NOT_PRESENT = 锁屏 / 屏保 / 显示器关闭：全部暂停（挂机时解码开销归零）
    let user_away = quns == QUNS_NOT_PRESENT;

    let fullscreen_labels = if user_away {
        Vec::new()
    } else {
        detect_fullscreen_labels(monitors)
    };

    let mut state = pause_state().lock().map_err(|_| "状态锁异常".to_string())?;
    for (i, m) in monitors.iter().enumerate() {
        let label = wallpaper::monitor_label(m, i);
        let should_pause = user_away || fullscreen_labels.contains(&label);
        if state.get(&label) != Some(&should_pause) {
            app.emit_to(&label, "wallpaper:fullscreen", should_pause)
                .map_err(|e| format!("发送暂停事件失败: {e}"))?;
            println!(
                "[fullscreen] {label} {}",
                if should_pause {
                    "已暂停"
                } else {
                    "已恢复"
                }
            );
            state.insert(label, should_pause);
        }
    }
    Ok(())
}

/// 功能关闭时确保所有屏恢复播放
fn resume_all(app: &AppHandle, monitors: &[Monitor]) -> Result<(), String> {
    let mut state = pause_state().lock().map_err(|_| "状态锁异常".to_string())?;
    for (i, m) in monitors.iter().enumerate() {
        let label = wallpaper::monitor_label(m, i);
        if state.get(&label) == Some(&true) {
            app.emit_to(&label, "wallpaper:fullscreen", false)
                .map_err(|e| format!("发送恢复事件失败: {e}"))?;
            println!("[fullscreen] {label} 已恢复（功能关闭）");
            state.insert(label, false);
        }
    }
    Ok(())
}

fn list_monitors(app: &AppHandle) -> Result<Vec<Monitor>, String> {
    // 同 wallpaper::list_monitors：不能依赖主窗口，静默启动时它还没有 WebView2
    app.available_monitors()
        .map_err(|e| format!("枚举显示器失败: {e}"))
}

/// 找出被应用窗口完全占据的屏幕，返回对应 label。支持多块屏同时全屏。
///
/// 判定：存在可见窗口**完整覆盖该屏工作区**（rcWork）。
/// 刻意不区分「最大化」与「真全屏」（曾试过 rcMonitor + WS_MAXIMIZE
/// 排除法，多屏布局下两路都漏检：任务栏只在主屏时，副屏的 rcWork==
/// rcMonitor，最大化的窗口覆满整屏却被 WS_MAXIMIZE 位排除；主屏最大化
/// 的窗口又盖不满含任务栏的 rcMonitor——结果用户全屏两个应用 GPU
/// 占用纹丝不动，功能失效）。本质回归：壁纸位于一切窗口之下，工作区
/// 被盖住时壁纸已 100% 不可见（主屏剩余条被不透明任务栏盖住），暂停
/// 没有任何视觉损失、纯省 GPU。最大化窗口的边框会超出屏幕约 8px，
/// 用覆盖而非精确重合；无边框全屏、F11、游戏天然满足。
///
/// 排除项：工具窗口、被 cloak 的窗口、Shell 系统层——
/// Progman/WorkerW（桌面本身）、任务栏、输入法与任务视图悬浮层
/// （CoreWindow/XamlExplorerHost）都天然铺满屏幕，会误报。
fn detect_fullscreen_labels(monitors: &[Monitor]) -> Vec<String> {
    // 每块屏的工作区
    let mut work_areas: Vec<(String, RECT)> = Vec::new();
    for (i, m) in monitors.iter().enumerate() {
        let pos = m.position();
        let size = m.size();
        let center = POINT {
            x: pos.x + size.width as i32 / 2,
            y: pos.y + size.height as i32 / 2,
        };
        unsafe {
            let hmon = MonitorFromPoint(center, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO::default();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(hmon, &mut info).as_bool() {
                work_areas.push((wallpaper::monitor_label(m, i), info.rcWork));
            }
        }
    }

    let mut labels = Vec::new();
    for hwnd in unsafe { collect_top_level_windows() } {
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                continue;
            }
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
                continue;
            }
            // 被 cloak 的窗口（UWP 挂起、切走的虚拟桌面）不可见，不算
            let mut cloaked = 0u32;
            if DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut c_void,
                std::mem::size_of::<u32>() as u32,
            )
            .is_ok()
                && cloaked != 0
            {
                continue;
            }
            let class = window_class_name(hwnd);
            if matches!(
                class.as_str(),
                "Progman"
                    | "WorkerW"
                    | "Shell_TrayWnd"
                    | "Shell_SecondaryTrayWnd"
                    | "Windows.UI.Core.CoreWindow"
                    | "XamlExplorerHostIslandWindow"
            ) {
                continue;
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                continue;
            }
            for (label, work) in &work_areas {
                if rect.left <= work.left
                    && rect.top <= work.top
                    && rect.right >= work.right
                    && rect.bottom >= work.bottom
                {
                    labels.push(label.clone());
                    break;
                }
            }
        }
    }
    labels
}

unsafe fn collect_top_level_windows() -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::new();
    let _ = EnumWindows(
        Some(enum_trampoline),
        LPARAM(&mut out as *mut Vec<HWND> as isize),
    );
    out
}

unsafe extern "system" fn enum_trampoline(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let out = &mut *(lparam.0 as *mut Vec<HWND>);
    out.push(hwnd);
    BOOL(1)
}

fn window_class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机诊断：枚举「覆盖了自己所在显示器工作区或整屏」的可见顶层窗口，
    /// 打印类名/标题/样式位/覆盖关系——排查「应用全屏了却不触发暂停」。
    /// 跑法：cargo test diagnose_fullscreen_detection -- --nocapture
    /// （需要用户保持问题窗口状态时执行）
    #[test]
    fn diagnose_fullscreen_detection() {
        use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONEAREST};
        use windows::Win32::UI::WindowsAndMessaging::{
            GetWindowTextW, GWL_STYLE, WS_CAPTION, WS_MAXIMIZE, WS_POPUP,
        };

        let covers = |r: &RECT, w: &RECT| {
            r.left <= w.left && r.top <= w.top && r.right >= w.right && r.bottom >= w.bottom
        };

        unsafe {
            for hwnd in collect_top_level_windows() {
                if !IsWindowVisible(hwnd).as_bool() {
                    continue;
                }
                let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
                    continue;
                }
                let mut cloaked = 0u32;
                if DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAKED,
                    &mut cloaked as *mut u32 as *mut c_void,
                    std::mem::size_of::<u32>() as u32,
                )
                .is_ok()
                    && cloaked != 0
                {
                    continue;
                }
                let class = window_class_name(hwnd);
                if matches!(
                    class.as_str(),
                    "Progman"
                        | "WorkerW"
                        | "Shell_TrayWnd"
                        | "Shell_SecondaryTrayWnd"
                        | "Windows.UI.Core.CoreWindow"
                        | "XamlExplorerHostIslandWindow"
                ) {
                    continue;
                }
                let mut rect = RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_err() {
                    continue;
                }
                // 窗口主要落在哪块屏，取该屏的 rcWork / rcMonitor 比对
                let hmon = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
                let mut info = MONITORINFO::default();
                info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
                if !GetMonitorInfoW(hmon, &mut info).as_bool() {
                    continue;
                }
                let covers_work = covers(&rect, &info.rcWork);
                let covers_mon = covers(&rect, &info.rcMonitor);
                if !covers_work && !covers_mon {
                    continue; // 只关心覆盖型窗口
                }
                let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as usize;
                let mut buf = [0u16; 128];
                let n = GetWindowTextW(hwnd, &mut buf);
                let title = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
                println!(
                    "[fs-diag] class={class:28} title={title:20} max={} popup={} caption={} coversWork={} coversMonitor={} rect=({},{})-({},{})",
                    style & WS_MAXIMIZE.0 as usize != 0,
                    style & WS_POPUP.0 as usize != 0,
                    style & WS_CAPTION.0 as usize != 0,
                    covers_work,
                    covers_mon,
                    rect.left,
                    rect.top,
                    rect.right,
                    rect.bottom
                );
            }
        }
    }
}
