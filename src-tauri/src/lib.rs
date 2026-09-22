mod fullscreen;
mod gallery;
mod probe;
mod settings;
mod transcode;
mod wallpaper;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Listener, Manager,
};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(tauri::generate_handler![
            wallpaper::set_wallpaper_on,
            wallpaper::unset_wallpaper_on,
            wallpaper::unset_wallpaper,
            wallpaper::get_screens,
            wallpaper::get_wallpaper_media,
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
            transcode::get_backup_dir
        ])
        .setup(|app| {
            // 壁纸窗口懒创建：按需建、闲置销毁（内存优化）；
            // 启动恢复放到后台线程，避免阻塞 setup 且与前端首屏请求竞态
            wallpaper::start_restore(app.handle().clone());
            // 壁纸页会上报自身布局（视口/视频矩形），打印到 dev 控制台用于诊断
            app.listen_any("wallpaper:debug", |event| {
                println!("[wallpaper-debug] {}", event.payload());
            });
            setup_tray(app)?;
            setup_main_window(app)?;
            // 全屏检测轮询线程：全屏应用占据某屏时暂停该屏壁纸
            fullscreen::start_poller(app.handle().clone());
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

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
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
