//! 分平台材质确认协议：只有原生调用成功才允许前端露出透明标题带。

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TitlebarEffect {
    Solid,
    #[cfg(any(target_os = "macos", test))]
    Vibrancy,
    #[cfg(any(target_os = "windows", test))]
    Acrylic,
}

pub fn apply(window: &tauri::WebviewWindow, dark: bool) -> TitlebarEffect {
    #[cfg(target_os = "macos")]
    let (result, effect) = (
        crate::native_titlebar::update(window, Some(dark), true),
        TitlebarEffect::Vibrancy,
    );
    #[cfg(target_os = "windows")]
    let (result, effect) = (
        crate::windows_titlebar::update(window, dark),
        TitlebarEffect::Acrylic,
    );
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    match result {
        Ok(true) => effect,
        Ok(false) => TitlebarEffect::Solid,
        Err(error) => {
            #[cfg(target_os = "windows")]
            crate::windows_titlebar::clear(window);
            tracing::warn!("原生标题栏材质不可用，回退实色：{error}");
            TitlebarEffect::Solid
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Linux 合成器和 WebKit 透明实现差异较大，默认实色，不猜测桌面环境。
        let _ = (window, dark);
        TitlebarEffect::Solid
    }
}

#[cfg(any(target_os = "windows", test))]
pub fn windows_block_reason(
    composition: bool,
    transparency: bool,
    high_contrast: bool,
) -> Option<&'static str> {
    if !composition {
        Some("DWM 合成未启用")
    } else if high_contrast {
        Some("系统开启了高对比度")
    } else if !transparency {
        Some("系统关闭了透明效果")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_names_are_stable_for_frontend_confirmation() {
        for (effect, name) in [
            (TitlebarEffect::Solid, "solid"),
            (TitlebarEffect::Vibrancy, "vibrancy"),
            (TitlebarEffect::Acrylic, "acrylic"),
        ] {
            assert_eq!(serde_json::to_value(effect).unwrap(), name);
        }
    }

    #[test]
    fn acrylic_requires_composition_transparency_and_no_high_contrast() {
        for composition in [false, true] {
            for transparency in [false, true] {
                for high_contrast in [false, true] {
                    assert_eq!(
                        windows_block_reason(composition, transparency, high_contrast).is_none(),
                        composition && transparency && !high_contrast,
                    );
                }
            }
        }
    }
}
