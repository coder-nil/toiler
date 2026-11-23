//! 界面缩放只改变 WebView 与标题栏布局，不改变原生窗口尺寸或最大化状态。

use std::sync::atomic::{AtomicU16, Ordering};

use tauri::Manager;

const DEFAULT_PERCENT: u16 = 100;
const MIN_PERCENT: u16 = 50;
const MAX_PERCENT: u16 = 300;
const STEP_PERCENT: u16 = 10;

/// 桌面服务每次启动使用随机端口；不要把倍率放在随 origin 变化的 localStorage。
/// 同一主窗口内重新加载或打开完整历史时保持倍率，重启应用时回到 100%。
pub struct InterfaceZoom(AtomicU16);

impl Default for InterfaceZoom {
    fn default() -> Self {
        Self(AtomicU16::new(DEFAULT_PERCENT))
    }
}

impl InterfaceZoom {
    #[cfg(target_os = "macos")]
    pub fn factor(&self) -> f64 {
        factor(self.0.load(Ordering::Relaxed))
    }
}

#[derive(Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZoomAction {
    In,
    Out,
    Reset,
    Restore,
}

impl ZoomAction {
    fn target(self, current: u16) -> u16 {
        match self {
            Self::In => (current + STEP_PERCENT).min(MAX_PERCENT),
            Self::Out => (current - STEP_PERCENT).max(MIN_PERCENT),
            Self::Reset => DEFAULT_PERCENT,
            Self::Restore => current,
        }
    }
}

fn factor(percent: u16) -> f64 {
    f64::from(percent) / 100.0
}

#[tauri::command]
pub async fn adjust_interface_zoom(
    window: tauri::WebviewWindow,
    action: ZoomAction,
) -> Result<u16, String> {
    if window.label() != "main" {
        return Err("仅允许调整主窗口的界面倍率".into());
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let target = window.clone();
    window
        .run_on_main_thread(move || {
            let _ = sender.send(apply(&target, action));
        })
        .map_err(|error| error.to_string())?;
    receiver.await.map_err(|error| error.to_string())?
}

fn apply(window: &tauri::WebviewWindow, action: ZoomAction) -> Result<u16, String> {
    let state = window.state::<InterfaceZoom>();
    let current = state.0.load(Ordering::Relaxed);
    let percent = action.target(current);
    if current != percent || matches!(action, ZoomAction::Restore) {
        // 让引擎调整视口、布局和命中坐标，避免 CSS 根容器缩放后要逐个修正浮层与分栏拖拽。
        window
            .set_zoom(factor(percent))
            .map_err(|error| format!("调整 WebView 倍率失败：{error}"))?;
        state.0.store(percent, Ordering::Relaxed);
        #[cfg(target_os = "macos")]
        crate::refresh_native_titlebar_geometry(window);
    }
    Ok(percent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_clamps_and_reset_is_independent_of_previous_scale() {
        let mut percent = DEFAULT_PERCENT;
        assert_eq!(ZoomAction::In.target(percent), 110);
        assert_eq!(ZoomAction::Out.target(percent), 90);
        for _ in 0..30 {
            percent = ZoomAction::Out.target(percent);
        }
        assert_eq!(percent, MIN_PERCENT);
        assert_eq!(ZoomAction::Restore.target(percent), MIN_PERCENT);
        for _ in 0..30 {
            percent = ZoomAction::In.target(percent);
        }
        assert_eq!(percent, MAX_PERCENT);
        assert_eq!(ZoomAction::Reset.target(percent), DEFAULT_PERCENT);
        assert_eq!(factor(50), 0.5);
        assert_eq!(factor(300), 3.0);
        assert_eq!(InterfaceZoom::default().0.load(Ordering::Relaxed), 100);
    }
}
