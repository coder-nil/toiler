//! Windows 11 22H2+ 的官方 DWM Acrylic。旧版 Windows 不调用未公开的
//! SetWindowCompositionAttribute，避免 Windows 10 上的拖动卡顿和假成功。

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null_mut;

use windows_sys::Win32::{
    Foundation::{
        ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, HWND, LPARAM, LRESULT, WPARAM,
    },
    Graphics::Dwm::{
        DwmExtendFrameIntoClientArea, DwmIsCompositionEnabled, DwmSetWindowAttribute, DWMSBT_NONE,
        DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    },
    System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD},
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        Controls::MARGINS,
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            GetWindowLongW, SetWindowLongW, SetWindowPos, SystemParametersInfoW, GWL_STYLE,
            SPI_GETHIGHCONTRAST, STYLESTRUCT, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
            SWP_NOSIZE, SWP_NOZORDER, WM_NCACTIVATE, WM_NCDESTROY, WM_NCPAINT, WM_STYLECHANGING,
            WS_CAPTION,
        },
    },
};

const CUSTOM_CAPTION_SUBCLASS: usize = 1;

fn without_caption(style: u32) -> u32 {
    style & !WS_CAPTION
}

unsafe extern "system" fn custom_caption_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    // SAFETY: STYLESTRUCT 仅在处理对应消息时有效；不保存指针。其它消息传给 Tao。
    unsafe {
        match message {
            // Tao 已用 WM_NCCALCSIZE 把标题栏区域交给 WebView，不再让 Win32 重绘旧窗框。
            WM_NCPAINT => 0,
            // -1 只禁止 DefWindowProc 重绘，仍让 Tao 处理激活状态与焦点事件。
            WM_NCACTIVATE => DefSubclassProc(hwnd, message, wparam, -1),
            WM_STYLECHANGING if wparam as i32 == GWL_STYLE && lparam != 0 => {
                let result = DefSubclassProc(hwnd, message, wparam, lparam);
                let styles = &mut *(lparam as *mut STYLESTRUCT);
                styles.styleNew = without_caption(styles.styleNew);
                result
            }
            WM_NCDESTROY => {
                RemoveWindowSubclass(hwnd, Some(custom_caption_proc), id);
                DefSubclassProc(hwnd, message, wparam, lparam)
            }
            _ => DefSubclassProc(hwnd, message, wparam, lparam),
        }
    }
}

/// 必须在窗口线程调用。Tao 的无边框模式只改客户区，仍保留 WS_CAPTION；
/// 扩展 DWM 背景时可能画出系统按钮。Tao 在显示、最大化等操作时会重新写样式，
/// 所以不能只在建窗时清一次。保留其它样式，避免影响边缘缩放和系统菜单。
pub fn install_custom_caption(window: &tauri::WebviewWindow) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|error| error.to_string())?.0 as HWND;
    install_custom_caption_on_hwnd(hwnd)
}

fn install_custom_caption_on_hwnd(hwnd: HWND) -> Result<(), String> {
    // SAFETY: 仅用于当前线程拥有的有效 HWND；回调不持有借用或外部状态。
    unsafe {
        if SetWindowSubclass(hwnd, Some(custom_caption_proc), CUSTOM_CAPTION_SUBCLASS, 0) == 0 {
            return Err("无法安装窗口标题栏样式回调".into());
        }
        let old = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        let new = without_caption(old);
        if old != new {
            // 旧样式有 WS_CAPTION，非零；SetWindowLongW 返回零即失败。
            if SetWindowLongW(hwnd, GWL_STYLE, new as i32) == 0
                || SetWindowPos(
                    hwnd,
                    null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_FRAMECHANGED | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
                ) == 0
            {
                return Err(format!(
                    "刷新自绘标题栏样式失败：{}",
                    std::io::Error::last_os_error()
                ));
            }
        }
    }
    Ok(())
}

fn check(result: i32, api: &str) -> Result<(), String> {
    if result < 0 {
        Err(format!("{api} 失败：HRESULT=0x{:08X}", result as u32))
    } else {
        Ok(())
    }
}

fn attribute(hwnd: HWND, kind: i32, value: i32) -> Result<(), String> {
    // SAFETY: HWND 由 Tauri 提供，传入的值和长度均为 DWM 要求的 32 位整数。
    check(
        unsafe {
            DwmSetWindowAttribute(
                hwnd,
                kind as u32,
                (&value as *const i32).cast::<c_void>(),
                size_of::<i32>() as u32,
            )
        },
        "DwmSetWindowAttribute",
    )
}

fn transparency_enabled() -> Result<bool, String> {
    let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = "EnableTransparency\0".encode_utf16().collect();
    let mut enabled: u32 = 0;
    let mut length = size_of::<u32>() as u32;
    // SAFETY: 两个 UTF-16 字符串均以 NUL 结尾，输出缓冲区及长度在调用期间有效。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            (&mut enabled as *mut u32).cast(),
            &mut length,
        )
    };
    match status {
        ERROR_SUCCESS => Ok(enabled != 0),
        // Windows 未写入个性化覆盖时沿用系统默认开启；其他读取错误保守降级。
        ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(true),
        _ => Err(format!("读取系统透明效果开关失败：{status}")),
    }
}

fn high_contrast_enabled() -> Result<bool, String> {
    let mut settings = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: cbSize 与结构体实际大小一致，输出指针在调用期间有效。
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            settings.cbSize,
            (&mut settings as *mut HIGHCONTRASTW).cast(),
            0,
        )
    };
    if ok == 0 {
        return Err("读取系统高对比度设置失败".into());
    }
    Ok(settings.dwFlags & HCF_HIGHCONTRASTON != 0)
}

pub fn update(window: &tauri::WebviewWindow, dark: bool) -> Result<bool, String> {
    let hwnd = window.hwnd().map_err(|error| error.to_string())?.0 as HWND;
    // SAFETY: 读取 Tauri 窗口的当前样式，不改变窗口状态。
    let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
    if style == 0 || style != without_caption(style) {
        return Err("无法确认系统标题栏已移除，不扩展 DWM 背景".into());
    }
    let mut composition = 0;
    // SAFETY: 输出 BOOL 指针有效，不修改用户设置。
    check(
        unsafe { DwmIsCompositionEnabled(&mut composition) },
        "DwmIsCompositionEnabled",
    )?;
    if let Some(reason) = crate::titlebar::windows_block_reason(
        composition != 0,
        transparency_enabled()?,
        high_contrast_enabled()?,
    ) {
        clear(window);
        tracing::info!("Windows 标题栏回退实色：{reason}");
        return Ok(false);
    }
    attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, i32::from(dark))?;
    // 只有支持官方 SystemBackdrop 的系统才启用 Acrylic；失败交由调用方清理并降级。
    attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_TRANSIENTWINDOW)?;
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    // SAFETY: HWND 有效；负边距是 DWM 官方的整窗背景扩展约定。
    check(
        unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) },
        "DwmExtendFrameIntoClientArea",
    )?;
    // DWM 材质覆盖原生窗口，前端仅在标题带留透明，正文仍绘制实色背景。
    tracing::info!("Windows 标题栏材质：Acrylic，页面深色={dark}，已启用官方 DWM 背景");
    Ok(true)
}

pub fn clear(window: &tauri::WebviewWindow) {
    let Ok(handle) = window.hwnd() else { return };
    let hwnd = handle.0 as HWND;
    if let Err(error) = attribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, DWMSBT_NONE) {
        // 老系统可能连清理属性都不支持，但前端仍以实色遮住透明窗口。
        tracing::debug!("清理 DWM 背景属性：{error}");
    }
    let margins = MARGINS {
        cxLeftWidth: 0,
        cxRightWidth: 0,
        cyTopHeight: 0,
        cyBottomHeight: 0,
    };
    // SAFETY: 恢复当前无边框窗口的普通客户区，不改变窗口 alpha。
    let result = unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) };
    if result < 0 {
        tracing::debug!("恢复 DWM 客户区失败：0x{:08X}", result as u32);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, SendMessageW, WS_MAXIMIZE, WS_MAXIMIZEBOX, WS_MINIMIZE,
        WS_MINIMIZEBOX, WS_SYSMENU, WS_THICKFRAME, WS_VISIBLE,
    };

    const WINDOW_FEATURES: u32 = WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;

    struct TestWindow(HWND);

    impl Drop for TestWindow {
        fn drop(&mut self) {
            // SAFETY: 测试窗口在当前测试线程创建，且未交给其它所有者。
            unsafe { DestroyWindow(self.0) };
        }
    }

    unsafe extern "system" fn observe_frame_messages(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        _data: usize,
    ) -> LRESULT {
        match message {
            WM_NCPAINT => 42,
            WM_NCACTIVATE if lparam == -1 => 43,
            WM_NCACTIVATE => 44,
            // SAFETY: 其它消息沿原窗口回调链传递，不读取参数指针。
            _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
        }
    }

    #[test]
    fn windows_caption_subclass_blocks_frame_paint_but_forwards_activation() {
        // 使用隐藏的测试 HWND 验证实际 Win32 消息链，不启动应用或依赖 WebView2。
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        // SAFETY: 系统预注册的 STATIC 类名以 NUL 结尾；未传入父窗口或创建数据。
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                WS_CAPTION | WINDOW_FEATURES,
                0,
                0,
                400,
                300,
                null_mut(),
                null_mut(),
                null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null(), "{}", std::io::Error::last_os_error());
        let _window = TestWindow(hwnd);
        // SAFETY: HWND 归当前线程；两条非客户区消息都不携带指针。
        unsafe {
            assert_ne!(
                SetWindowSubclass(hwnd, Some(observe_frame_messages), 1, 0),
                0
            );
            assert_eq!(SendMessageW(hwnd, WM_NCPAINT, 1, 0), 42);
            assert_eq!(SendMessageW(hwnd, WM_NCACTIVATE, 1, 0), 44);

            install_custom_caption_on_hwnd(hwnd).unwrap();
            assert_eq!(GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_CAPTION, 0);
            assert_eq!(SendMessageW(hwnd, WM_NCPAINT, 1, 0), 0);
            for active in [0, 1] {
                assert_eq!(SendMessageW(hwnd, WM_NCACTIVATE, active, 0), 43);
            }
            // 模拟 Tao 在最大化或还原时重写样式，不把系统标题栏带回来。
            for state in [WS_MAXIMIZE, 0] {
                let style = WS_CAPTION | WINDOW_FEATURES | state;
                assert_ne!(SetWindowLongW(hwnd, GWL_STYLE, style as i32), 0);
                let actual = GetWindowLongW(hwnd, GWL_STYLE) as u32;
                assert_eq!(actual & WS_CAPTION, 0);
                assert_eq!(
                    actual & (WINDOW_FEATURES | WS_MAXIMIZE),
                    WINDOW_FEATURES | state
                );
            }
        }
    }

    #[test]
    fn tao_style_updates_keep_window_features_without_system_caption() {
        let features = WINDOW_FEATURES;
        for state in [
            0,
            WS_VISIBLE,
            WS_VISIBLE | WS_MAXIMIZE,
            WS_VISIBLE | WS_MINIMIZE,
        ] {
            let expected = features | state;
            assert_eq!(without_caption(expected | WS_CAPTION), expected);
            assert_eq!(without_caption(expected), expected);
        }
    }
}
