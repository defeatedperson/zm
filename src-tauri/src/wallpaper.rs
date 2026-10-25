use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Manager, Monitor, WebviewUrl, WebviewWindowBuilder};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, CreateRectRgn, RedrawWindow, SetWindowRgn, RDW_ALLCHILDREN, RDW_ERASE,
    RDW_INVALIDATE, RDW_UPDATENOW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, GetClassNameW, GetClientRect, GetParent, GetWindow,
    GetWindowLongPtrW, GetWindowRect, SendMessageTimeoutW, SetParent, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, GWL_EXSTYLE, GWL_STYLE, GW_CHILD, GW_HWNDNEXT, SMTO_NORMAL,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, WS_BORDER,
    WS_CAPTION, WS_CHILD, WS_DLGFRAME, WS_POPUP, WS_THICKFRAME, WS_EX_CLIENTEDGE,
    WS_EX_DLGMODALFRAME, WS_EX_WINDOWEDGE,
};

use crate::gallery;
use crate::settings;

/// 向 Progman 发送该消息后，系统会确保桌面图标层（SHELLDLL_DefView）后面
/// 存在 WorkerW 窗口，它是动态壁纸的宿主层。未文档化消息（社区称
/// WM_SHELLPARENTCHANGING），Win10 22H2 实测仍有效，但生成层级形态不唯一，
/// 见 [find_or_spawn_workerw] 的注释。
const WM_SPAWN_WORKERW: u32 = 0x052C;

/// 每块显示器对应一个壁纸窗口，标签统一以该前缀开头
const WALLPAPER_PREFIX: &str = "wallpaper-";

/// 壁纸窗口专用的 WebView2 浏览器参数。
///
/// `CalculateNativeWinOcclusion` 必须关掉：壁纸窗口是 WorkerW 的子窗口，
/// 常年被判定为「完全遮挡」，Chromium 会因此把页面标记为 hidden 并节流——
/// rAF / requestVideoFrameCallback / timeupdate 都会变稀疏甚至停摆，
/// 循环切点就此错过帧边界，表现为卡顿与衔接不准。
///
/// 注意：WebView2 的浏览器参数只在**浏览器进程创建时**生效，而全部窗口共用
/// 同一进程 —— 谁先创建谁说了算（主窗口先于壁纸窗口）。所以主窗口在
/// tauri.conf.json 里也配了同一份参数，两处必须保持一致。
///
/// 前面几项是 wry 的默认值，`additional_browser_args` 会整体覆盖它，
/// 因此必须一并带上；`--autoplay-policy` 同理（wry 原本会自动追加）。
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,\
CalculateNativeWinOcclusion --autoplay-policy=no-user-gesture-required";

/// 启动恢复是否已完成（get_screens 需等待它，避免把恢复中间态返回给前端）
static RESTORE_DONE: AtomicBool = AtomicBool::new(false);

/// 用户手动暂停（仅冻结画面，不卸载壁纸；会话级，重启后恢复播放）
static USER_PAUSED: AtomicBool = AtomicBool::new(false);

/// 当前是否处于用户暂停
pub fn user_paused() -> bool {
    USER_PAUSED.load(Ordering::Relaxed)
}

/// 切换用户暂停：广播给所有壁纸窗口，并同步托盘菜单文字
pub fn apply_user_paused(app: &AppHandle, paused: bool) -> Result<(), String> {
    USER_PAUSED.store(paused, Ordering::Relaxed);
    app.emit("wallpaper:user-paused", paused)
        .map_err(|e| format!("发送暂停事件失败: {e}"))?;
    if let Some(tray) = app.try_state::<crate::TrayMenus>() {
        let _ = tray
            .pause_item
            .set_text(if paused { "恢复播放" } else { "暂停壁纸" });
    }
    Ok(())
}

/// 当前每块屏正在播放的壁纸路径（label -> path），供 UI 展示与删除保护
fn wallpaper_state() -> &'static Mutex<HashMap<String, String>> {
    static STATE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 最近一次设置的壁纸路径（托盘「启动动态壁纸」重新应用；清除后仍保留）
fn last_wallpaper() -> &'static Mutex<Option<String>> {
    static LAST: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// 指定路径是否正被任一屏幕使用
pub fn is_wallpaper_in_use(path: &str) -> bool {
    wallpaper_state()
        .lock()
        .map(|state| state.values().any(|p| p == path))
        .unwrap_or(false)
}

/// 是否有任一屏幕的壁纸处于活动状态（含仅暂停未卸载的情况）。
/// 迁移存储位置等操作的前置检查用。
pub fn any_active(app: &AppHandle) -> bool {
    let state_active = wallpaper_state()
        .lock()
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    state_active
        || app
            .webview_windows()
            .keys()
            .any(|label| label.starts_with(WALLPAPER_PREFIX))
}

/// 清掉内存中的「最近壁纸」记录（迁移存储位置后旧路径已失效）
pub fn clear_last_wallpaper() {
    if let Ok(mut last) = last_wallpaper().lock() {
        *last = None;
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenInfo {
    pub label: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub wallpaper_path: Option<String>,
}

pub fn monitor_label(m: &Monitor, index: usize) -> String {
    if let Some(name) = m.name() {
        let cleaned: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !cleaned.is_empty() {
            return format!("{WALLPAPER_PREFIX}{cleaned}");
        }
    }
    format!("{WALLPAPER_PREFIX}{index}")
}

fn list_monitors(app: &AppHandle) -> Result<Vec<Monitor>, String> {
    let main = app.get_webview_window("main").ok_or("主窗口未创建")?;
    main.available_monitors()
        .map_err(|e| format!("枚举显示器失败: {e}"))
}

/// 确保指定屏幕的壁纸窗口存在（懒创建：只建需要的那块屏，闲置屏不占用 webview）。
///
/// 关键点：窗口必须从创建那一刻起就落在自己的显示器上、以该屏的 DPI 初始化。
/// 因为窗口挂入 WorkerW 成为子窗口后，系统不会再对它补发 DPI 变更消息，
/// 创建时的 DPI 就是最终渲染 DPI；先建在主屏再挪走会导致渲染比例与
/// 物理尺寸对不上，出现边缘空白。
fn ensure_window(app: &AppHandle, label: &str, m: &Monitor) -> Result<(), String> {
    if app.get_webview_window(label).is_some() {
        return Ok(());
    }
    let scale = m.scale_factor();
    // tauri builder 的 position/inner_size 接受逻辑坐标：物理值 / DPI 缩放
    let x = m.position().x as f64 / scale;
    let y = m.position().y as f64 / scale;
    let w = m.size().width as f64 / scale;
    let h = m.size().height as f64 / scale;
    WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::App("wallpaper.html".into()),
    )
    .title("LivePaper Wallpaper")
    .decorations(false)
    .resizable(false)
    .skip_taskbar(true)
    .visible(false)
    .additional_browser_args(BROWSER_ARGS)
    .position(x, y)
    .inner_size(w, h)
    .build()
    .map_err(|e| format!("创建壁纸窗口 {label} 失败: {e}"))?;
    Ok(())
}

/// 把窗口的全部后代窗口（WebView2 的各层宿主）强制铺满整个窗口区域。
/// WebView2 渲染面默认按“客户区”摆放，某些情况下客户区与窗口存在边框差值，
/// 逐层物理拍平最直接可靠。
unsafe fn force_fill_children(hwnd: HWND, width: i32, height: i32) {
    let mut child = match GetWindow(hwnd, GW_CHILD) {
        Ok(c) if c.0 as usize != 0 => c,
        _ => return,
    };
    let mut guard = 0;
    while guard < 16 {
        let _ = SetWindowPos(
            child,
            None,
            0,
            0,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        force_fill_children(child, width, height);
        child = match GetWindow(child, GW_HWNDNEXT) {
            Ok(next) if next.0 as usize != 0 => next,
            _ => break,
        };
        guard += 1;
    }
}

/// 挂载失败的统一文案：说明原因 + 给出用户可执行的操作。
/// 该文案会透传到主窗口底部状态栏（红字），面向普通用户而非开发者。
fn attach_unavailable(reason: &str) -> String {
    format!(
        "{reason}，已停止挂载动态壁纸（可能与 Wallpaper Engine 等壁纸软件冲突，或 explorer 刚重启）。\
         请完全退出其他壁纸软件后重试；若仍失败，请重启资源管理器（explorer）后再试"
    )
}

/// 读取窗口类名，失败返回 None。
unsafe fn class_name(hwnd: HWND) -> Option<String> {
    let mut buf = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut buf);
    if n > 0 {
        Some(String::from_utf16_lossy(&buf[..n as usize]))
    } else {
        None
    }
}

/// 定位桌面图标层背后的 WorkerW 宿主层（必要时先生成）。
///
/// 任何一步失败都必须返回 Err 并放弃挂载：把壁纸窗口直接挂到 Progman
/// 正面会盖住桌面图标层（SHELLDLL_DefView），表现为"整个桌面只剩
/// 任务栏可见"——任务栏是独立 topmost 窗口不受影响，图标层却会被
/// 完全遮住。宁可不出壁纸，也不能盖住用户的桌面。
///
/// 桌面窗口层级是未文档化行为，实测至少三种形态并存：
/// - A：DefView 与 0x052C 生成的 WorkerW 都是 Progman 的子窗口（旧假设）
/// - B：DefView 被重新挂到顶级 WorkerW 下（Win8+ 发送 0x052C 后、系统启用
///   幻灯片壁纸时、Wallpaper Engine 等接管后），壁纸挂其后的顶级 WorkerW
/// - C：DefView 是 Progman 子窗口，但 WorkerW 是顶级窗口、Z 序在 Progman 后
///
/// 因此与同类软件（Wallpaper Engine / Lively / MSDN 官方示例）保持一致：
/// 先找 DefView 所在的宿主（不预设 Progman 还是 WorkerW），再按兄弟 Z 序、
/// 顶级 Z 序两步定位挂载点，全程带退避重试（WorkerW 生成存在异步时机，
/// explorer 忙碌或刚重启时会延迟出现）。
unsafe fn find_or_spawn_workerw() -> Result<HWND, String> {
    let progman = FindWindowW(w!("Progman"), PCWSTR::null())
        .map_err(|_| attach_unavailable("未找到桌面窗口 Progman"))?;
    let mut last_err = String::new();
    // 首轮立即尝试，之后退避重试，给 explorer 留出构建桌面层级的时间
    for delay_ms in [0u64, 100, 300, 600, 1000] {
        if delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        // 幂等操作：WorkerW 已存在时重复发送无副作用
        let _ = SendMessageTimeoutW(
            progman,
            WM_SPAWN_WORKERW,
            WPARAM(0),
            LPARAM(0),
            SMTO_NORMAL,
            1000,
            None,
        );
        match locate_host_layer(progman) {
            Ok(host) => return Ok(host),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// 在上述各种桌面层级形态下定位可挂载的 WorkerW。
unsafe fn locate_host_layer(progman: HWND) -> Result<HWND, String> {
    // 桌面图标层的宿主：形态 A/C 下是 Progman，形态 B 下是一个顶级 WorkerW。
    // 两种都查不到说明图标层不存在（用户关闭了"显示桌面图标"，
    // 或 explorer 正在重建桌面），无法安全定位挂载点
    let defview_host = find_defview_host(progman)
        .ok_or_else(|| attach_unavailable("未找到桌面图标层 SHELLDLL_DefView"))?;
    let defview = FindWindowExW(
        Some(defview_host),
        None,
        w!("SHELLDLL_DefView"),
        PCWSTR::null(),
    )
    .map_err(|_| attach_unavailable("未找到桌面图标层 SHELLDLL_DefView"))?;
    // 第一步（形态 A）：兄弟 Z 序 —— DefView 同级的 WorkerW，即宿主的子窗口。
    // 不直接取第一个兄弟：其他壁纸工具可能在图标层后面插入自己的窗口，
    // 挂到那种窗口上等于挂错宿主
    let mut cur = defview;
    for _ in 0..16 {
        let Ok(next) = GetWindow(cur, GW_HWNDNEXT) else {
            break;
        };
        if next.0 as usize == 0 {
            break;
        }
        if class_name(next).as_deref() == Some("WorkerW") {
            return Ok(next);
        }
        cur = next;
    }
    // 第二步（形态 B/C）：顶级 Z 序 —— DefView 宿主之后的第一个顶级 WorkerW。
    // MSDN 官方示例 / Wallpaper Engine / Lively 的通用做法
    FindWindowExW(None, Some(defview_host), w!("WorkerW"), PCWSTR::null())
        .map_err(|_| attach_unavailable("桌面图标层之后不存在可挂载的 WorkerW"))
}

/// 找到桌面图标层 SHELLDLL_DefView 的宿主顶级窗口：
/// 先看 Progman 直接子窗口（形态 A/C），再枚举顶级 WorkerW（形态 B）。
unsafe fn find_defview_host(progman: HWND) -> Option<HWND> {
    if FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), PCWSTR::null()).is_ok() {
        return Some(progman);
    }
    let mut after: Option<HWND> = None;
    while let Ok(worker) = FindWindowExW(None, after, w!("WorkerW"), PCWSTR::null()) {
        if FindWindowExW(Some(worker), None, w!("SHELLDLL_DefView"), PCWSTR::null()).is_ok() {
            return Some(worker);
        }
        after = Some(worker);
    }
    None
}

/// 把每块显示器的壁纸窗口挂到 WorkerW 之下，用物理像素精确贴合该屏矩形。
/// 每屏一个窗口、窗口内铺满视口，因此天然规避跨屏 DPI 换算误差。
/// `only` 传入时只挂载标签匹配的那块屏（单屏应用壁纸）。
fn attach_wallpaper_windows(app: &AppHandle, only: Option<&str>) -> Result<(), String> {
    println!("[wallpaper] attach v3 (force-fill){}", only.map(|l| format!(" [{l}]")).unwrap_or_default());
    unsafe {
        let host = find_or_spawn_workerw()?;
        let mut host_rect = RECT::default();
        GetWindowRect(host, &mut host_rect).map_err(|e| format!("获取桌面区域失败: {e}"))?;
        println!(
            "[wallpaper] WorkerW rect: left={} top={} right={} bottom={}",
            host_rect.left, host_rect.top, host_rect.right, host_rect.bottom
        );

        for (i, m) in list_monitors(app)?.iter().enumerate() {
            let label = monitor_label(m, i);
            if let Some(target) = only {
                if label != target {
                    continue;
                }
            }
            // 懒创建：需要挂载的屏如果没有窗口，先建
            ensure_window(app, &label, m)?;
            let Some(win) = app.get_webview_window(&label) else {
                continue;
            };
            let raw = win.hwnd().map_err(|e| format!("获取窗口句柄失败: {e}"))?;
            let hwnd = HWND(raw.0);
            println!(
                "[wallpaper] {label}: monitor={:?} pos=({},{}) size={}x{} scale={} dpi={}",
                m.name(),
                m.position().x,
                m.position().y,
                m.size().width,
                m.size().height,
                m.scale_factor(),
                GetDpiForWindow(hwnd),
            );

            // 无边框弹出窗口 → 子窗口。必须把 tao 为实现阴影而保留的
            // WS_CAPTION/WS_THICKFRAME 等边框样式连同扩展样式一起剥干净：
            // 作为子窗口时 tao 的"抹边框"消息处理不再生效，系统会按残留
            // 样式算出约 8px 非客户区，WebView 只能铺到客户区、四周露边
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as usize;
            let frames = (WS_POPUP.0 | WS_CAPTION.0 | WS_THICKFRAME.0 | WS_BORDER.0
                | WS_DLGFRAME.0) as usize;
            let new_style = (style & !frames) | WS_CHILD.0 as usize;
            SetWindowLongPtrW(hwnd, GWL_STYLE, new_style as isize);

            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as usize;
            let ex_frames =
                (WS_EX_CLIENTEDGE.0 | WS_EX_WINDOWEDGE.0 | WS_EX_DLGMODALFRAME.0) as usize;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex_style & !ex_frames) as isize);

            SetParent(hwnd, Some(host)).map_err(|e| format!("挂载到桌面层失败: {e}"))?;
            SetWindowPos(
                hwnd,
                None,
                m.position().x - host_rect.left,
                m.position().y - host_rect.top,
                m.size().width as i32,
                m.size().height as i32,
                SWP_SHOWWINDOW | SWP_FRAMECHANGED,
            )
            .map_err(|e| format!("调整壁纸窗口位置失败: {e}"))?;

            // tao 窗口过程对 WM_NCCALCSIZE 的处理会让客户区相对窗口
            // 缩进一圈（本机实测左8/顶1/右8/底8），且无法通过剥离窗口
            // 样式消除。对策：实测缩进量后反向平移放大宿主窗口，让
            // 客户区精确覆盖显示器矩形，WebView 铺客户区即严丝合缝
            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            let mut origin = POINT { x: 0, y: 0 };
            let _ = ClientToScreen(hwnd, &mut origin);
            let mut win = RECT::default();
            let _ = GetWindowRect(hwnd, &mut win);
            let dx = origin.x - win.left;
            let dy = origin.y - win.top;
            let right_gap = (win.right - win.left) - (client.right - client.left) - dx;
            let bottom_gap = (win.bottom - win.top) - (client.bottom - client.top) - dy;
            println!(
                "[wallpaper] {label}: client inset dx={dx} dy={dy} right={right_gap} bottom={bottom_gap}"
            );
            let _ = SetWindowPos(
                hwnd,
                None,
                // 子窗口坐标是父窗口（WorkerW）相对坐标，需换算回虚拟屏幕原点
                m.position().x - dx - host_rect.left,
                m.position().y - dy - host_rect.top,
                m.size().width as i32 + dx + right_gap,
                m.size().height as i32 + dy + bottom_gap,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );

            let mut after = RECT::default();
            let _ = GetWindowRect(hwnd, &mut after);
            println!(
                "[wallpaper] {label}: window rect after: left={} top={} right={} bottom={}",
                after.left, after.top, after.right, after.bottom
            );
            // 强制 WebView2 各层宿主铺满客户区（= 显示器矩形）
            force_fill_children(
                hwnd,
                m.size().width as i32,
                m.size().height as i32,
            );

            // 把窗口可见区域裁剪为客户区：补偿产生的外沿会伸进相邻屏幕，
            // 压住邻居壁纸窗口的边缘像素（表现为邻屏边缘的细线/空条）。
            // 注意：补偿完成后客户区恰好等于显示器矩形，直接用显示器尺寸
            let hrgn = CreateRectRgn(
                dx,
                dy,
                dx + m.size().width as i32,
                dy + m.size().height as i32,
            );
            let _ = SetWindowRgn(hwnd, Some(hrgn), true);
        }
    }
    Ok(())
}

/// 隐藏壁纸窗口并脱离桌面层，恢复系统静态壁纸。
/// `only` 传入时只处理标签匹配的那块屏。
fn detach_wallpaper_windows(app: &AppHandle, only: Option<&str>) -> Result<(), String> {
    // 被卸载壁纸的宿主 WorkerW，全部卸完后统一触发重绘
    let mut hosts: Vec<HWND> = Vec::new();
    for win in app.webview_windows().values() {
        if !win.label().starts_with(WALLPAPER_PREFIX) {
            continue;
        }
        if let Some(target) = only {
            if win.label() != target {
                continue;
            }
        }
        let raw = win.hwnd().map_err(|e| format!("获取窗口句柄失败: {e}"))?;
        let hwnd = HWND(raw.0);
        unsafe {
            // 必须先隐藏再恢复窗口形态：若在可见状态下 SetWindowRgn(…, true)
            // 会触发一次整窗重绘，而窗口随即被销毁，个别机器上 DWM 来不及
            // 回收这一帧，就在桌面顶部留下条状残影（形似窗口标题栏，切换
            // 系统壁纸才被冲掉）。隐藏后恢复区域无需重绘
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = SetWindowRgn(hwnd, None, false);
            // 记录宿主，稍后统一强制重绘
            if let Ok(host) = GetParent(hwnd) {
                if !hosts.contains(&host) {
                    hosts.push(host);
                }
            }
            // 恢复为顶层无边框窗口，保持下次挂载前的干净状态
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as usize;
            let new_style = (style & !(WS_CHILD.0 as usize)) | WS_POPUP.0 as usize;
            SetWindowLongPtrW(hwnd, GWL_STYLE, new_style as isize);
            let _ = SetParent(hwnd, None);
        }
    }
    // 壁纸窗口消失后，宿主 WorkerW 不会自动重绘其原覆盖区域，可能继续
    // 显示 DWM 缓存的最后一帧。显式失效整棵宿主树强制立即重绘，确保
    // 静态壁纸马上透出来（同类软件退出时的通用处理）
    for host in hosts {
        unsafe {
            let _ = RedrawWindow(
                Some(host),
                None,
                None,
                RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN | RDW_UPDATENOW,
            );
        }
    }
    Ok(())
}

/// 按扩展名分流：图片发 set-image，视频发 set-video
fn set_video_on_window(app: &AppHandle, label: &str, path: String) -> Result<(), String> {
    let event = if gallery::is_image(std::path::Path::new(&path)) {
        "wallpaper:set-image"
    } else {
        "wallpaper:set-video"
    };
    app.emit_to(label, event, path)
        .map_err(|e| format!("发送媒体路径失败: {e}"))
}

/// 在指定屏幕（label）上应用壁纸；label 为 None 时应用到所有屏幕。
/// 注意必须为 async：同步命令在主线程执行，而懒创建窗口需要投递回主线程，
/// 会造成自等死锁；async 命令在线程池执行，无此问题。
#[tauri::command]
pub async fn set_wallpaper_on(
    app: AppHandle,
    path: String,
    label: Option<String>,
) -> Result<(), String> {
    if crate::storage::is_migrating() {
        return Err("正在迁移存储位置，请稍候".into());
    }
    attach_wallpaper_windows(&app, label.as_deref())?;
    let mut state = wallpaper_state().lock().map_err(|_| "状态锁异常".to_string())?;
    if let Some(l) = &label {
        set_video_on_window(&app, l, path.clone())?;
        state.insert(l.clone(), path.clone());
        settings::update_assignments(&app, |m| {
            m.insert(l.clone(), path.clone());
        });
    } else {
        // 应用到全部：给每块屏都发视频
        let mut labels = Vec::new();
        for (i, m) in list_monitors(&app)?.iter().enumerate() {
            labels.push(monitor_label(m, i));
        }
        state.clear();
        for l in &labels {
            set_video_on_window(&app, l, path.clone())?;
            state.insert(l.clone(), path.clone());
        }
        settings::update_assignments(&app, |m| {
            m.clear();
            for l in &labels {
                m.insert(l.clone(), path.clone());
            }
        });
    }
    // 记录最近一次壁纸，供托盘重新应用
    if let Ok(mut last) = last_wallpaper().lock() {
        *last = Some(path);
    }
    Ok(())
}

/// 托盘：把最近一次设置的壁纸重新应用到全部屏幕
pub async fn reapply_last(app: &AppHandle) -> Result<(), String> {
    let Some(path) = last_wallpaper()
        .lock()
        .map_err(|_| "状态锁异常".to_string())?
        .clone()
    else {
        return Err("本次运行尚未设置过动态壁纸".into());
    };
    set_wallpaper_on(app.clone(), path, None).await
}

/// 退出前把壁纸窗口从桌面层卸下，恢复系统静态壁纸
pub fn shutdown_cleanup(app: &AppHandle) {
    let _ = detach_wallpaper_windows(app, None);
}

/// setup 里调用：后台线程执行启动恢复。
/// 不能留在 setup 里同步跑——建窗口会泵消息队列，主窗口页面可能趁隙
/// 调用 get_screens 拿到恢复到一半的状态。
pub fn start_restore(app: AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        restore_from_settings(&app);
        RESTORE_DONE.store(true, Ordering::Release);
        // 存量视频的元数据回填（缺 sidecar 的补探测），完成后广播刷新
        gallery::backfill_meta(&app);
    });
}

/// 启动时按持久化的分配恢复每屏壁纸（文件已删除或屏幕不存在的项自动跳过）
pub fn restore_from_settings(app: &AppHandle) {
    let settings = settings::load(app);
    if settings.wallpaper_assignments.is_empty() {
        return;
    }
    println!(
        "[wallpaper] 恢复上次的壁纸分配（{} 项）",
        settings.wallpaper_assignments.len()
    );
    let mut last_path: Option<String> = None;
    let monitors = match list_monitors(app) {
        Ok(m) => m,
        Err(e) => {
            println!("[wallpaper] 恢复失败：{e}");
            return;
        }
    };
    for (i, m) in monitors.iter().enumerate() {
        let label = monitor_label(m, i);
        let Some(path) = settings.wallpaper_assignments.get(&label) else {
            continue;
        };
        if !std::path::Path::new(path).is_file() {
            println!("[wallpaper] 跳过 {label}：文件不存在 {path}");
            continue;
        }
        // 挂载（内部懒创建窗口）；页面加载完成会通过 get_wallpaper_media 拉取自己的壁纸
        if let Err(e) = attach_wallpaper_windows(app, Some(&label)) {
            println!("[wallpaper] 恢复 {label} 挂载失败: {e}");
            // 后台线程静默失败用户无从感知，广播给主窗口状态栏红字提示
            let _ = app.emit("wallpaper:attach-error", format!("启动恢复动态壁纸失败：{e}"));
            continue;
        }
        if let Ok(mut state) = wallpaper_state().lock() {
            state.insert(label.clone(), path.clone());
        }
        last_path = Some(path.clone());
        println!("[wallpaper] 已恢复 {label}");
    }
    // 托盘「启动动态壁纸」可重新应用恢复的壁纸
    if let (Some(p), Ok(mut last)) = (last_path, last_wallpaper().lock()) {
        *last = Some(p);
    }
}

/// 壁纸页面就绪后拉取本屏的媒体分配（懒创建窗口时 emit 会早于页面监听注册而丢失）。
/// 返回 None 表示本屏当前无壁纸。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    pub path: String,
    /// "video" | "image"
    pub kind: String,
    /// 拉取时该屏是否正处于铺满暂停（新建窗口需要补上这个状态）
    pub fullscreen_paused: bool,
    /// 拉取时是否处于用户暂停
    pub user_paused: bool,
}

#[tauri::command]
pub fn get_wallpaper_media(label: String) -> Option<MediaInfo> {
    let state = wallpaper_state().lock().ok()?;
    let path = state.get(&label)?.clone();
    drop(state);
    let kind = gallery::media_kind(std::path::Path::new(&path)).to_string();
    Some(MediaInfo {
        path,
        kind,
        fullscreen_paused: crate::fullscreen::is_paused(&label),
        user_paused: user_paused(),
    })
}

/// 查询用户暂停状态
#[tauri::command]
pub fn get_user_paused() -> bool {
    user_paused()
}

/// 切换用户暂停（仅冻结画面，不卸载壁纸）
#[tauri::command]
pub fn set_user_paused(app: AppHandle, paused: bool) -> Result<(), String> {
    apply_user_paused(&app, paused)
}

/// 在指定屏幕（label）上恢复静态壁纸；label 为 None 时清除所有屏幕。
/// 清除后销毁对应窗口：闲置屏不再保留 webview（内存优化）。
#[tauri::command]
pub async fn unset_wallpaper_on(app: AppHandle, label: Option<String>) -> Result<(), String> {
    let mut state = wallpaper_state().lock().map_err(|_| "状态锁异常".to_string())?;
    if let Some(l) = &label {
        let _ = app.emit_to(l, "wallpaper:clear", ());
        state.remove(l);
        // 卸下并销毁窗口，彻底释放该屏的 webview 资源
        detach_wallpaper_windows(&app, Some(l))?;
        if let Some(win) = app.get_webview_window(l) {
            let _ = win.destroy();
        }
        settings::update_assignments(&app, |m| {
            m.remove(l);
        });
    } else {
        let _ = app.emit("wallpaper:clear", ());
        state.clear();
        detach_wallpaper_windows(&app, None)?;
        // 销毁所有壁纸窗口
        let labels: Vec<String> = app
            .webview_windows()
            .into_iter()
            .filter(|(name, _)| name.starts_with(WALLPAPER_PREFIX))
            .map(|(name, _)| name)
            .collect();
        for l in labels {
            if let Some(win) = app.get_webview_window(&l) {
                let _ = win.destroy();
            }
        }
        settings::update_assignments(&app, |m| m.clear());
    }
    Ok(())
}

/// 取消动态壁纸，恢复静态壁纸（全屏）。
#[tauri::command]
pub async fn unset_wallpaper(app: AppHandle) -> Result<(), String> {
    unset_wallpaper_on(app, None).await
}

/// 列出所有显示器及其当前壁纸状态。
/// async + 等待启动恢复完成，保证前端拿到的状态是完整的。
#[tauri::command]
pub async fn get_screens(app: AppHandle) -> Result<Vec<ScreenInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !RESTORE_DONE.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        build_screens(&app)
    })
    .await
    .map_err(|e| format!("任务调度失败: {e}"))?
}

fn build_screens(app: &AppHandle) -> Result<Vec<ScreenInfo>, String> {
    let state = wallpaper_state().lock().map_err(|_| "状态锁异常".to_string())?;
    let mut out = Vec::new();
    for (i, m) in list_monitors(app)?.iter().enumerate() {
        let label = monitor_label(m, i);
        let size = m.size();
        out.push(ScreenInfo {
            name: m.name().map(|s| s.to_string()).unwrap_or_else(|| format!("显示器 {}", i + 1)),
            wallpaper_path: state.get(&label).cloned(),
            label,
            width: size.width,
            height: size.height,
        });
    }
    Ok(out)
}
