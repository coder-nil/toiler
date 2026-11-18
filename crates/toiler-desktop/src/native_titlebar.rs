//! macOS 标题带的实时窗口后方材质。
//!
//! 只创建一个具名 NSVisualEffectView，放在 WebView 下方。采样和刷新交给
//! WindowServer；不截图、不轮询屏幕、不改变窗口或材质视图的 alpha。

use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAutoresizingMaskOptions, NSColor,
    NSUserInterfaceItemIdentification, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowOrderingMode, NSWorkspace,
};
use objc2_foundation::{ns_string, NSPoint, NSRect, NSSize};

const EFFECT_ID: &str = "toiler-live-titlebar";

fn titlebar_frame(bounds: NSRect, flipped: bool) -> NSRect {
    let height = bounds.size.height.clamp(0.0, crate::TITLEBAR_HEIGHT);
    let y = if flipped {
        bounds.origin.y
    } else {
        bounds.origin.y + bounds.size.height.max(0.0) - height
    };
    NSRect::new(
        NSPoint::new(bounds.origin.x, y),
        NSSize::new(bounds.size.width.max(0.0), height),
    )
}

fn effect_view(content: &NSView) -> Result<Retained<NSVisualEffectView>, String> {
    for child in content.subviews() {
        if child
            .identifier()
            .is_some_and(|id| id.to_string() == EFFECT_ID)
        {
            return child
                .downcast::<NSVisualEffectView>()
                .map_err(|_| "标题栏材质标识被其他视图占用".to_string());
        }
    }
    let main = MainThreadMarker::new().ok_or("原生材质只能在主线程创建")?;
    let effect = NSVisualEffectView::new(main);
    effect.setIdentifier(Some(ns_string!("toiler-live-titlebar")));
    // 不把 WebView 放进材质视图，否则其文字也可能受到 vibrancy 影响。
    content.addSubview_positioned_relativeTo(&effect, NSWindowOrderingMode::Below, None);
    Ok(effect)
}

/// 所有入口都在主线程执行；布局更新复用原有材质视图，不重建采样层。
pub fn update(
    window: &tauri::WebviewWindow,
    dark: Option<bool>,
    diagnostics: bool,
) -> Result<bool, String> {
    let _main = MainThreadMarker::new().ok_or("原生材质只能在主线程更新")?;
    let raw = window.ns_window().map_err(|error| error.to_string())?;
    // SAFETY: 指针来自同一窗口的 ns_window，且上面已确认当前是主线程。
    let native: &NSWindow = unsafe { &*(raw as *mut NSWindow) };
    native.setOpaque(false);
    native.setBackgroundColor(Some(&NSColor::clearColor()));
    native.setTitlebarAppearsTransparent(true);
    let content = native.contentView().ok_or("窗口没有 contentView")?;
    let effect = effect_view(&content)?;
    let frame = titlebar_frame(content.bounds(), content.isFlipped());
    effect.setFrame(frame);
    effect.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable
            | if content.isFlipped() {
                NSAutoresizingMaskOptions::ViewMaxYMargin
            } else {
                NSAutoresizingMaskOptions::ViewMinYMargin
            },
    );
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setMaterial(NSVisualEffectMaterial::Sidebar);
    // 后方视频或其他窗口变化时，即便 Toiler 失焦也保留系统材质合成。
    effect.setState(NSVisualEffectState::Active);
    let reduce_transparency =
        NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency();
    effect.setHidden(reduce_transparency);
    if let Some(dark) = dark {
        let name = if dark {
            ns_string!("NSAppearanceNameDarkAqua")
        } else {
            ns_string!("NSAppearanceNameAqua")
        };
        let appearance = NSAppearance::appearanceNamed(name).ok_or("系统不支持标题栏主题")?;
        effect.setAppearance(Some(&appearance));
    }
    if diagnostics {
        let count = content
            .subviews()
            .iter()
            .filter(|view| {
                view.identifier()
                    .is_some_and(|id| id.to_string() == EFFECT_ID)
            })
            .count();
        tracing::info!(
            "实时标题栏材质：数量={count}，区域={:?}，模式={:?}，材质={:?}，状态={:?}，alpha={}，已关联窗口={}，窗口不透明={}，页面深色={dark:?}，实际主题={}，降低透明度={reduce_transparency}",
            effect.frame(), effect.blendingMode(), effect.material(), effect.state(), effect.alphaValue(),
            effect.window().is_some(), native.isOpaque(), effect.effectiveAppearance().name(),
        );
        if reduce_transparency {
            tracing::warn!(
                "系统开启了降低透明度，原生材质可能显示为实色；应用不会擅自修改辅助功能设置"
            );
        }
    }
    Ok(!reduce_transparency)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titlebar_is_anchored_to_top_in_both_coordinate_systems() {
        let bounds = NSRect::new(NSPoint::new(7.0, 11.0), NSSize::new(800.0, 600.0));
        assert_eq!(
            titlebar_frame(bounds, false).origin,
            NSPoint::new(7.0, 569.0)
        );
        assert_eq!(titlebar_frame(bounds, true).origin, NSPoint::new(7.0, 11.0));
        assert_eq!(titlebar_frame(bounds, false).size, NSSize::new(800.0, 42.0));
    }

    #[test]
    fn resizing_changes_width_and_origin_but_not_titlebar_height() {
        for (width, height) in [(720.0, 520.0), (1280.0, 820.0), (1920.0, 1080.0)] {
            let frame = titlebar_frame(
                NSRect::new(NSPoint::ZERO, NSSize::new(width, height)),
                false,
            );
            assert_eq!(frame.size, NSSize::new(width, crate::TITLEBAR_HEIGHT));
            assert_eq!(frame.origin.y + frame.size.height, height);
        }
    }

    #[test]
    fn temporarily_empty_or_short_content_never_produces_negative_size() {
        for height in [0.0, 20.0, -1.0] {
            let frame = titlebar_frame(NSRect::new(NSPoint::ZERO, NSSize::new(0.0, height)), false);
            assert_eq!(frame.origin.y, 0.0);
            assert!(frame.size.height >= 0.0 && frame.size.height <= height.max(0.0));
        }
    }
}
