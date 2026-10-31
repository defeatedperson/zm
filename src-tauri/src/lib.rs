mod fullscreen;
mod gallery;
mod probe;
mod settings;
mod storage;
mod transcode;
mod wallpaper;

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Listener, Manager,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegGetValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ,
};

/// 开机自启的静默启动标记：由 Run 值带给 exe，命中则启动后不弹主窗口（只留托盘）。
/// 必须与 tauri_plugin_autostart::init 传给 auto-launch 的参数一致。
const HIDDEN_FLAG: &str = "--hidden";

/// 当前用户的开机自启注册表位置（HKCU 无需管理员权限）
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// 本次是否以静默方式启动（开机自启）
static LAUNCHED_HIDDEN: AtomicBool = AtomicBool::new(false);

/// 静默启动的 WebView2 预热窗口标签
const WARMUP_LABEL: &str = "warmup";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 单实例锁：必须放在插件链最前面（窗口/托盘创建前拿锁）。
        // 第二个实例启动即退出，并把自身命令行交给首个实例
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // 开机自启那次启动带 --hidden，不该把窗口弹出来；
            // 用户手动再开一次（无参数）才显示主窗口
            if !has_hidden_flag(&args) {
                show_main_window(app);
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_FLAG]),
        ))
        .invoke_handler(tauri::generate_handler![
            wallpaper::set_wallpaper_on,
            wallpaper::unset_wallpaper_on,
            wallpaper::unset_wallpaper,
            wallpaper::get_screens,
            wallpaper::get_wallpaper_media,
            wallpaper::get_last_attach_error,
            wallpaper::get_user_paused,
            wallpaper::set_user_paused,
            settings::get_settings,
            settings::set_fullscreen_pause,
            settings::open_path_in_explorer,
            settings::open_url_in_browser,
            gallery::list_gallery,
            gallery::add_wallpaper,
            gallery::remove_wallpaper,
            gallery::save_thumbnail,
            transcode::optimize_start,
            transcode::optimize_cancel,
            transcode::get_backup_dir,
            storage::migrate_storage,
            storage::delete_default_dirs
        ])
        .setup(|app| {
            // 启动场景判定：开机自启（Run 值带 --hidden）静默起，只留托盘图标。
            // 用 args_os 而不是 args()：后者遇到非 UTF-8 参数会 panic
            LAUNCHED_HIDDEN.store(
                std::env::args_os().any(|a| a.to_string_lossy() == HIDDEN_FLAG),
                Ordering::Relaxed,
            );
            // 老版本写下的自启项没有 --hidden，而 NSIS 更新会原样保留旧值
            // → 每次启动做一次幂等规范化，老用户升级后自动静默。
            // 安装器侧（nsis-hooks.nsh 的 POSTINSTALL）会在装完时就规范化一次，
            // 这里是运行时兜底；纯卸载不会清理该值——NSIS 卸载器只删自己写的
            // 内容，残留项指向不存在的 exe 时 Windows 会静默忽略，无害
            repair_autostart_entry(app.handle());
            // 配置了自定义存储位置时，把新位置加入 asset 协议运行时白名单。
            // 必须在启动恢复创建壁纸窗口（加载视频）之前完成
            storage::extend_runtime_scope(app.handle());
            // 壁纸窗口懒创建：按需建、闲置销毁（内存优化）；
            // 启动恢复放到后台线程，避免阻塞 setup 且与前端首屏请求竞态
            //
            // 静默启动要先建好 WebView2 预热窗口：恢复流程紧接着就会创建壁纸窗口，
            // 而隐藏创建的窗口没有可见创建的窗口打底就拿不到窗口句柄
            if launched_hidden() {
                if let Err(e) = create_warmup_window(app.handle()) {
                    println!("[warmup] {e}");
                }
            }
            wallpaper::start_restore(app.handle().clone());
            // 壁纸页会上报自身布局（视口/视频矩形），打印到 dev 控制台用于诊断
            app.listen_any("wallpaper:debug", |event| {
                println!("[wallpaper-debug] {}", event.payload());
            });
            setup_tray(app)?;
            setup_main_window(app)?;
            // 全屏检测轮询线程：全屏应用占据某屏时暂停该屏壁纸
            fullscreen::start_poller(app.handle().clone());
            // 只有手动启动才显示主窗口；开机自启全程不碰它（见 show_main_window 注释）
            if !launched_hidden() {
                show_main_window(app.handle());
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 托盘菜单项引用：暂停项文字需要随状态切换
pub struct TrayMenus {
    pub pause_item: tauri::menu::MenuItem<tauri::Wry>,
}

/// 系统托盘：显示主窗口 / 暂停 / 启动（重新应用上次壁纸）/ 停止 / 退出
fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let pause = MenuItem::with_id(app, "pause", "暂停壁纸", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", "启动动态壁纸", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "恢复静态壁纸", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&show, &pause, &sep1, &start, &stop, &sep2, &quit])?;
    app.manage(TrayMenus { pause_item: pause });

    let mut builder = TrayIconBuilder::with_id("main-tray")
        .tooltip("幻梦桌面")
        .menu(&menu)
        // 左键单击托盘图标 = 显示主窗口；右键弹出菜单
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "pause" => {
                // 仅冻结画面，不卸载壁纸；无窗口创建操作，主线程直接执行即可
                if let Err(e) = wallpaper::apply_user_paused(app, !wallpaper::user_paused()) {
                    println!("[tray] 切换暂停失败: {e}");
                }
            }
            "start" => {
                // 托盘事件在主线程回调，窗口懒创建必须走异步运行时，避免主线程自等死锁
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = wallpaper::reapply_last(&handle).await {
                        println!("[tray] 启动动态壁纸失败: {e}");
                    }
                });
            }
            "stop" => {
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = wallpaper::unset_wallpaper_on(handle, None).await {
                        println!("[tray] 恢复静态壁纸失败: {e}");
                    }
                });
            }
            "quit" => {
                // 退出前卸载壁纸层，恢复系统静态壁纸
                wallpaper::shutdown_cleanup(app);
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// 显示主窗口（托盘、单实例回调、静默启动失败时都走这里）。
///
/// 主窗口在配置里是 visible=false：静默启动时它压根不出现（既不占桌面也不占任务栏），
/// 首次调用本函数（show）才会创建窗口句柄与 WebView2，所以第一次从托盘打开面板
/// 是冷启动（约 1s 内加载完）。能这么做的前提是：
/// - 枚举显示器不再依赖主窗口（见 wallpaper::list_monitors）；
/// - 静默启动另有预热窗口维持 WebView2 环境（见 create_warmup_window）。
pub fn show_main_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        // 主窗口一旦建出来就常驻（隐藏到托盘也保留 WebView2），
        // 从此它就是预热环境的载体，预热窗口可以退场
        destroy_warmup_window(app);
    }
}

/// 静默启动（开机自启）时创建 WebView2 预热窗口。
///
/// 背景：只有「可见创建」过的窗口才会拿到 WebView2 控制器与窗口句柄，而壁纸窗口
/// 为了不闪屏是 hidden 创建的。手动启动时主窗口就是那个可见创建的窗口；
/// 开机自启不显示主窗口，就没有任何东西把 WebView2 环境带起来，壁纸窗口会永远
/// 拿不到句柄、挂不进 WorkerW（实测重试 5 秒也不出现）。
///
/// 所以这里建一个纯占位的窗口，加载最轻的 wallpaper.html（查不到本屏分配就停在空状态），
/// 生命周期只到主窗口第一次被显示。位置与可见性处理：
/// - 完全放到屏幕外（-32000）会被系统丢弃定位、退回默认层叠位置（实测），反而更难看；
///   所以贴在主屏右下角，只露出约 8x8 像素，且通常正好被任务栏挡住；
/// - `build()` 返回时 WebView2 已就绪（建窗内部会泵消息等到控制器可用），
///   此时立刻 hide 是安全的——早于就绪去 hide 会打断初始化（实测窗口直接消失）。
fn create_warmup_window(app: &tauri::AppHandle) -> Result<(), String> {
    if app.get_webview_window(WARMUP_LABEL).is_some() {
        return Ok(());
    }
    // 只露 8px 的角落坐标：物理值 / DPI 缩放 = builder 要的逻辑坐标
    let (x, y) = match app.primary_monitor().ok().flatten() {
        Some(m) => {
            let scale = m.scale_factor();
            let right = m.position().x + m.size().width as i32 - 8;
            let bottom = m.position().y + m.size().height as i32 - 8;
            (right as f64 / scale, bottom as f64 / scale)
        }
        None => (0.0, 0.0),
    };
    let win = tauri::WebviewWindowBuilder::new(
        app,
        WARMUP_LABEL,
        tauri::WebviewUrl::App("wallpaper.html".into()),
    )
    .title("LivePaper Warmup")
    .decorations(false)
    .resizable(false)
    .skip_taskbar(true)
    .focused(false)
    .position(x, y)
    .inner_size(64.0, 64.0)
    // 静默启动时本窗口是最先创建的，WebView2 浏览器参数由它定调，
    // 必须与壁纸窗口/主窗口保持同一份（见 wallpaper::BROWSER_ARGS）
    .additional_browser_args(wallpaper::BROWSER_ARGS)
    .build()
    .map_err(|e| format!("创建预热窗口失败: {e}"))?;
    let _ = win.hide();
    Ok(())
}

/// 销毁预热窗口（没建过就是空操作）
fn destroy_warmup_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window(WARMUP_LABEL) {
        let _ = win.destroy();
    }
}

/// 主窗口关闭 = 隐藏到托盘（壁纸进程必须保持运行），真正退出走托盘菜单
fn setup_main_window(app: &tauri::App) -> tauri::Result<()> {
    if let Some(win) = app.get_webview_window("main") {
        let win_for_close = win.clone();
        win.on_window_event(move |event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = win_for_close.hide();
            }
        });
    }
    Ok(())
}

// ---- 开机自启静默启动 ----

/// 本次启动是否来自开机自启（静默）
pub fn launched_hidden() -> bool {
    LAUNCHED_HIDDEN.load(Ordering::Relaxed)
}

/// 命令行里是否带静默启动标记（首实例查自身 args，单实例回调查第二实例 args）
fn has_hidden_flag<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|a| a.as_ref() == HIDDEN_FLAG)
}

/// Rust 字符串 → 以 NUL 结尾的 UTF-16 缓冲区。
/// 返回的 Vec 必须在 Win32 调用期间存活（PCWSTR 只是裸指针）。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 读取 HKCU Run 下名为 value_name 的 REG_SZ 值；不存在或类型不符返回 None
unsafe fn read_run_value(value_name: &str) -> Option<String> {
    let sub_key = wide(RUN_KEY);
    let name = wide(value_name);
    // 两段式：先问长度，再取值
    let mut size = 0u32;
    let code = RegGetValueW(
        HKEY_CURRENT_USER,
        PCWSTR::from_raw(sub_key.as_ptr()),
        PCWSTR::from_raw(name.as_ptr()),
        RRF_RT_REG_SZ,
        None,
        None,
        Some(&mut size),
    );
    if code != ERROR_SUCCESS || size < 2 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    let code = RegGetValueW(
        HKEY_CURRENT_USER,
        PCWSTR::from_raw(sub_key.as_ptr()),
        PCWSTR::from_raw(name.as_ptr()),
        RRF_RT_REG_SZ,
        None,
        Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
        Some(&mut size),
    );
    if code != ERROR_SUCCESS || size as usize > buf.len() {
        return None;
    }
    // 长度不是 2 的倍数时（异常情况）丢掉最后半个字符
    let (pairs, _) = buf[..size as usize].as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|b| u16::from_le_bytes(*b)).collect();
    Some(
        String::from_utf16_lossy(&units)
            .trim_end_matches('\0')
            .to_string(),
    )
}

/// 把 value 写成 HKCU Run 下名为 value_name 的 REG_SZ；失败返回 Win32 错误码
unsafe fn write_run_value(value_name: &str, value: &str) -> Result<(), u32> {
    let sub_key = wide(RUN_KEY);
    let name = wide(value_name);
    let mut hkey = HKEY(std::ptr::null_mut());
    let code = RegOpenKeyExW(
        HKEY_CURRENT_USER,
        PCWSTR::from_raw(sub_key.as_ptr()),
        None,
        KEY_READ | KEY_SET_VALUE,
        &mut hkey,
    );
    if code != ERROR_SUCCESS {
        return Err(code.0);
    }
    // REG_SZ 要求数据含结尾 NUL
    let data: Vec<u8> = value
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .chain([0, 0])
        .collect();
    let code = RegSetValueExW(
        hkey,
        PCWSTR::from_raw(name.as_ptr()),
        None,
        REG_SZ,
        Some(&data),
    );
    let _ = RegCloseKey(hkey);
    if code != ERROR_SUCCESS {
        return Err(code.0);
    }
    Ok(())
}

/// 从 Run 值里取出 exe 路径：带引号取引号内内容；不带引号（auto-launch 0.5/0.6
/// 的写法）按第一个 " -" 切掉参数，剩余整体视为路径，
/// 这样用户名目录含空格也不会被截断。
fn exe_path_from_value(value: &str) -> Option<&str> {
    let value = value.trim_end();
    if let Some(rest) = value.strip_prefix('"') {
        let path = rest.split('"').next().unwrap_or_default();
        return (!path.is_empty()).then_some(path);
    }
    let path = match value.find(" -") {
        Some(i) => &value[..i],
        None => value,
    };
    (!path.is_empty()).then_some(path)
}

/// 把已存在的开机自启项规范化为 `"<exe>" --hidden`，一次修掉两件事：
/// - 旧版本没有 --hidden（升级后仍弹窗的根因：NSIS 更新会保留旧值，
///   新版安装器的 POSTINSTALL hook 已在装完时先行处理，这里是运行时兜底）
/// - auto-launch 拼值不给路径加引号（用户名目录含空格时自启直接失效）
///
/// 只改已存在的项、绝不新建（用户没开自启就不碰注册表）；不写 StartupApproved，
/// 用户在任务管理器里禁用启动项的选择保持原样。
/// exe 路径取自注册表现有值而不是 current_exe()：dev 下 current_exe() 是
/// target\\debug 里的临时二进制，写进去会把用户机器的启动项指歪。
/// 解析出的路径必须真实存在才写回：路径含 " -" 之类会被启发式截断、
/// 或用户换过安装目录时，宁可保持原值不动，也不能写一个指向空处的路径。
fn repair_autostart_entry(app: &tauri::AppHandle) {
    let name = app.package_info().name.clone();
    let Some(current) = (unsafe { read_run_value(&name) }) else {
        return;
    };
    let Some(exe) = exe_path_from_value(&current) else {
        println!("[autostart] 启动项内容无法解析，跳过规范化: {current}");
        return;
    };
    if !std::path::Path::new(exe).is_file() {
        println!("[autostart] 启动项指向的 exe 不存在，跳过规范化（换过目录或解析异常）: {exe}");
        return;
    }
    let desired = format!("\"{exe}\" {HIDDEN_FLAG}");
    if desired == current {
        return; // 幂等：已是最新形态就不再写注册表
    }
    match unsafe { write_run_value(&name, &desired) } {
        Ok(()) => println!("[autostart] 启动项已规范化: {desired}"),
        Err(code) => println!("[autostart] 规范化启动项失败（不影响本次运行）: Win32 错误码 {code}"),
    }
}
