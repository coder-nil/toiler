//! Windows 11 22H2+ 的官方 DWM Acrylic。旧版 Windows 不调用未公开的
//! SetWindowCompositionAttribute，避免 Windows 10 上的拖动卡顿和假成功。

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::null_mut;

use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS, HWND},
    Graphics::Dwm::{
        DwmExtendFrameIntoClientArea, DwmIsCompositionEnabled, DwmSetWindowAttribute, DWMSBT_NONE,
        DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    },
    System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD},
    UI::{
        Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW},
        Controls::MARGINS,
        WindowsAndMessaging::{SystemParametersInfoW, SPI_GETHIGHCONTRAST},
    },
};

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
