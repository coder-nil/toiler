//! 按本地月份轮换运行中的应用图标（北半球气象季节）。
//!
//! macOS 的窗口图标接口不会更新 Dock，必须在主线程设置 NSApplication 图标。
//! 不改应用包里的 icns：那会破坏签名，Finder 与未运行时仍使用原版品牌图标。

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Season {
    Spring,
    Summer,
    Autumn,
    Winter,
}

impl Season {
    fn from_month(month: u32) -> Option<Self> {
        match month {
            3..=5 => Some(Self::Spring),
            6..=8 => Some(Self::Summer),
            9..=11 => Some(Self::Autumn),
            12 | 1 | 2 => Some(Self::Winter),
            _ => None,
        }
    }

    fn png(self) -> &'static [u8] {
        match self {
            Self::Spring => include_bytes!("../icons/seasons/spring.png"),
            Self::Summer => include_bytes!("../icons/seasons/summer.png"),
            Self::Autumn => include_bytes!("../icons/seasons/autumn.png"),
            Self::Winter => include_bytes!("../icons/seasons/winter.png"),
        }
    }
}

#[derive(Default)]
pub(crate) struct SeasonalIconState {
    applied: Mutex<Option<Season>>,
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

fn current_season() -> Result<Season, String> {
    let seconds: libc::time_t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs()
        .try_into()
        .map_err(|_| "系统时间超出本地日历范围")?;
    // SAFETY: 零初始化后仅交给系统时间函数填充；各平台都用线程安全版本。
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    #[cfg(unix)]
    let valid = !unsafe { libc::localtime_r(&seconds, &mut local) }.is_null();
    #[cfg(windows)]
    let valid = unsafe { libc::localtime_s(&mut local, &seconds) } == 0;
    if !valid {
        return Err("读取本地月份失败".into());
    }
    Season::from_month((local.tm_mon + 1) as u32).ok_or_else(|| "本地月份不合法".into())
}

#[cfg(target_os = "macos")]
fn apply(_app: &AppHandle, season: Season) -> Result<(), String> {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let main_thread = MainThreadMarker::new().ok_or("Dock 图标只能在主线程更新")?;
    let data = NSData::with_bytes(season.png());
    let image = NSImage::initWithData(NSImage::alloc(), &data).ok_or("季节图标解码失败")?;
    // SAFETY: 在主线程提供有效的 NSImage，且始终传 Some，不使用空图标。
    unsafe {
        NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&image));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn apply(app: &AppHandle, season: Season) -> Result<(), String> {
    let icon = tauri::image::Image::from_bytes(season.png()).map_err(|error| error.to_string())?;
    for window in app.webview_windows().values() {
        window
            .set_icon(icon.clone())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Ready 和定时回调都在主线程执行；失败时保留旧图标，下次检查会重试。
fn refresh(app: &AppHandle) {
    let result = (|| {
        let season = current_season()?;
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
            // 每次重新读本地日历，休眠唤醒、跨年或修改系统时间后也会跟上。
            tokio::time::sleep(Duration::from_secs(60)).await;
            let inner = handle.clone();
            if let Err(error) = handle.run_on_main_thread(move || refresh(&inner)) {
                tracing::warn!("调度季节图标更新失败：{error}");
            }
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
    fn season_boundaries_cover_the_year_and_winter_wraps() {
        use Season::*;
        let expected = [
            Winter, Winter, Spring, Spring, Spring, Summer, Summer, Summer, Autumn, Autumn, Autumn,
            Winter,
        ];
        for (index, season) in expected.into_iter().enumerate() {
            assert_eq!(Season::from_month(index as u32 + 1), Some(season));
        }
        assert_eq!(Season::from_month(0), None);
        assert_eq!(Season::from_month(13), None);
    }

    #[test]
    fn embedded_icons_decode_with_transparent_corners() {
        for season in [
            Season::Spring,
            Season::Summer,
            Season::Autumn,
            Season::Winter,
        ] {
            let image = tauri::image::Image::from_bytes(season.png()).unwrap();
            assert_eq!((image.width(), image.height()), (512, 512));
            let alpha = |x: usize, y: usize| image.rgba()[(y * 512 + x) * 4 + 3];
            for (x, y) in [(0, 0), (511, 0), (0, 511), (511, 511)] {
                assert_eq!(alpha(x, y), 0, "{season:?} 的外围必须透明");
            }
            assert_eq!(alpha(256, 256), 255, "{season:?} 的主体不能被挖空");
        }
    }
}
