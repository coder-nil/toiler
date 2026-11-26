//! 按立春、立夏、立秋、立冬的天文计算时刻轮换运行中的应用图标。
//!
//! macOS 的窗口图标接口不会更新 Dock，必须在主线程设置 NSApplication 图标。
//! 不改应用包里的 icns：那会破坏签名，Finder 与未运行时仍使用原版品牌图标。

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use tauri::{AppHandle, Manager};
use toiler_server::seasons::{Calendar, Season};

fn png(season: Season) -> &'static [u8] {
    match season {
        Season::Spring => include_bytes!("../icons/seasons/spring.png"),
        Season::Summer => include_bytes!("../icons/seasons/summer.png"),
        Season::Autumn => include_bytes!("../icons/seasons/autumn.png"),
        Season::Winter => include_bytes!("../icons/seasons/winter.png"),
    }
}

#[derive(Default)]
pub(crate) struct SeasonalIconState {
    applied: Mutex<Option<Season>>,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    calendar: Mutex<Calendar>,
}

fn current_season(app: &AppHandle) -> Result<(Season, Duration), String> {
    let state = app.state::<SeasonalIconState>();
    let mut calendar = state.calendar.lock().map_err(|_| "节气日历状态不可用")?;
    calendar.at(SystemTime::now())
}

#[cfg(target_os = "macos")]
fn apply(_app: &AppHandle, season: Season) -> Result<(), String> {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let main_thread = MainThreadMarker::new().ok_or("Dock 图标只能在主线程更新")?;
    let data = NSData::with_bytes(png(season));
    let image = NSImage::initWithData(NSImage::alloc(), &data).ok_or("季节图标解码失败")?;
    // SAFETY: 在主线程提供有效的 NSImage，且始终传 Some，不使用空图标。
    unsafe {
        NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&image));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn apply(app: &AppHandle, season: Season) -> Result<(), String> {
    let icon = tauri::image::Image::from_bytes(png(season)).map_err(|error| error.to_string())?;
    for window in app.webview_windows().values() {
        #[cfg(target_os = "windows")]
        let hwnd = window.hwnd().map_err(|error| error.to_string())?;
        window
            .set_icon(icon.clone())
            .map_err(|error| error.to_string())?;
        #[cfg(target_os = "windows")]
        sync_taskbar_icon(hwnd.0 as _)?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn sync_taskbar_icon(hwnd: windows_sys::Win32::Foundation::HWND) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageW, ICON_BIG, ICON_SMALL, WM_GETICON, WM_SETICON,
    };

    // Tauri 在主线程同步设置小图标，任务栏则可读取大图标槽。
    // 两个槽共用 Tao 持有的 HICON；WM_SETICON 不转移所有权，不销毁返回的旧句柄。
    // SAFETY: hwnd 来自当前主线程上的有效窗口，这两条消息都不传缓冲区指针。
    let icon = unsafe {
        let icon = SendMessageW(hwnd, WM_GETICON, ICON_SMALL as _, 0);
        SendMessageW(hwnd, WM_SETICON, ICON_BIG as _, icon);
        icon
    };
    if icon == 0 {
        return Err("无法读取 Windows 窗口小图标".into());
    }
    Ok(())
}

/// Ready 和定时回调都在主线程执行；失败时保留旧图标，下次检查会重试。
fn refresh(app: &AppHandle) {
    let result = (|| {
        let (season, _) = current_season(app)?;
        let state = app.state::<SeasonalIconState>();
        let mut applied = state.applied.lock().map_err(|_| "季节图标状态不可用")?;
        if *applied != Some(season) {
            apply(app, season)?;
            *applied = Some(season);
            tracing::info!("已切换季节图标：{season:?}");
        }
        Ok::<_, String>(())
    })();
    if let Err(error) = result {
        tracing::warn!("切换季节图标失败：{error}");
    }
}

/// 必须在 RunEvent::Ready 中调用，晚于框架自己的默认图标初始化。
pub(crate) fn start(app: &AppHandle) {
    refresh(app);
    let handle = app.clone();
    let task = tauri::async_runtime::spawn(async move {
        loop {
            // 平时每分钟检查时钟，交节前则睡到具体时刻，不额外迟到一分钟。
            let delay = current_season(&handle)
                .map(|(_, delay)| delay)
                .unwrap_or(Duration::from_secs(60));
            let inner = handle.clone();
            if let Err(error) = handle.run_on_main_thread(move || refresh(&inner)) {
                tracing::warn!("调度季节图标更新失败：{error}");
            }
            tokio::time::sleep(delay).await;
        }
    });
    let state = app.state::<SeasonalIconState>();
    if let Ok(mut slot) = state.task.lock() {
        if let Some(previous) = slot.replace(task) {
            previous.abort();
        }
    };
}

pub(crate) fn stop(app: &AppHandle) {
    if let Some(state) = app.try_state::<SeasonalIconState>() {
        if let Ok(mut slot) = state.task.lock() {
            if let Some(task) = slot.take() {
                task.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icons_decode_with_transparent_corners() {
        for season in [
            Season::Spring,
            Season::Summer,
            Season::Autumn,
            Season::Winter,
        ] {
            let image = tauri::image::Image::from_bytes(png(season)).unwrap();
            assert_eq!((image.width(), image.height()), (512, 512));
            let alpha = |x: usize, y: usize| image.rgba()[(y * 512 + x) * 4 + 3];
            for (x, y) in [(0, 0), (511, 0), (0, 511), (511, 511)] {
                assert_eq!(alpha(x, y), 0, "{season:?} 的外围必须透明");
            }
            assert_eq!(alpha(256, 256), 255, "{season:?} 的主体不能被挖空");
        }
    }
}
