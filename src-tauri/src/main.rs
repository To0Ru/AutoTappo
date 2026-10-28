// Release 发布版禁用 Windows 控制台黑框
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod chest;
mod clicker;
mod commands;
mod session;
mod tray;
mod uia;
mod updater;
mod window;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tauri::tray::TrayIcon;
use tauri::Manager;

use crate::chest::ChestController;
use crate::clicker::ClickerController;

/// 全局共享状态
pub struct Inner {
    pub clicker: Arc<ClickerController>,
    pub chest: Arc<ChestController>,
    pub tray: Mutex<Option<TrayIcon>>,
    pub tray_ok: AtomicBool,
    pub shutting_down: AtomicBool,
}

pub struct AppState(pub Arc<Inner>);

fn main() {
    // 多屏 / 高 DPI 下必须拿到物理像素坐标，区域与截图像素才能对齐
    chest::ensure_dpi_aware();

    // 上一次一键更新遗留的旧版本备份（.bak）与未完成的下载（.new）在这里清理
    updater::cleanup_after_update();

    // 注册「显示器开关状态」电源通知：锁定 / 息屏时必须停手，
    // 否则我们注入的键击会把已经黑掉的显示器重新点亮（详见 session.rs 顶部说明）
    session::init();

    let app = tauri::Builder::default()
        .setup(|app| {
            let clicker = Arc::new(ClickerController::new());
            // 启动后台静默连点原生工作线程
            clicker.start_worker();

            // 自动开箱引擎：独立原生线程，默认关闭
            let chest = Arc::new(ChestController::new());
            chest.start_worker();

            let inner = Arc::new(Inner {
                clicker,
                chest,
                tray: Mutex::new(None),
                tray_ok: AtomicBool::new(false),
                shutting_down: AtomicBool::new(false),
            });
            app.manage(AppState(Arc::clone(&inner)));

            // 构造系统托盘图标
            match tray::build(app, Arc::clone(&inner)) {
                Ok(icon) => {
                    if let Ok(mut slot) = inner.tray.lock() {
                        *slot = Some(icon);
                    }
                    inner.tray_ok.store(true, Ordering::SeqCst);
                }
                Err(e) => {
                    eprintln!("[AutoTappo] 托盘图标创建失败: {e}");
                }
            }

            // 启动时默认打开主控制面板窗口
            let _ = window::ensure(app.handle());

            Ok(())
        })
        .on_window_event(|win, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = win.app_handle();
                let tray_ok = app.state::<AppState>().0.tray_ok.load(Ordering::SeqCst);
                if tray_ok {
                    // 点击右上角关闭：不退出程序，直接销毁窗口释放 WebView2 渲染内存
                    // 托盘图标依然常驻，点击托盘可随时秒级重新唤醒
                    api.prevent_close();
                    let _ = window::close(app);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::toggle_running,
            commands::set_running,
            commands::set_speed,
            commands::set_hotkey,
            commands::get_hotkey,
            commands::reset_sent_count,
            commands::minimize_to_tray,
            commands::quit_app,
            commands::get_chest_status,
            commands::set_auto_chest,
            commands::relaunch_as_admin,
            commands::apply_update,
            commands::cancel_update,
        ])
        .build(tauri::generate_context!())
        .expect("AutoTappo 启动失败");

    app.run(|handle, event| {
        // 关闭主窗口后窗口数为 0，Tauri 默认会请求退出。
        // 托盘常驻守护时必须拦住；只有用户在托盘菜单真正点击「退出」时才放行。
        if let tauri::RunEvent::ExitRequested { api, .. } = event {
            let state = handle.state::<AppState>();
            if !state.0.shutting_down.load(Ordering::SeqCst) && state.0.tray_ok.load(Ordering::SeqCst) {
                api.prevent_exit();
            }
        }
    });
}
