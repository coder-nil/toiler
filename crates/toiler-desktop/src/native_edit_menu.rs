//! macOS 的默认编辑菜单消费 ⌘Z，再发送原生 `undo:`；Monaco 的历史不在原生撤销栈里。

use objc2::{sel, MainThreadMarker};
use objc2_app_kit::NSApplication;
use tauri::menu::{Menu, MenuEvent, MenuItem, MenuItemKind};
use tauri::{AppHandle, Manager, Wry};

const UNDO_ID: &str = "toiler.edit.undo";
const REDO_ID: &str = "toiler.edit.redo";

pub fn build(app: &AppHandle<Wry>) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::default(app)?;
    for item in menu.items()? {
        let MenuItemKind::Submenu(edit) = item else {
            continue;
        };
        if edit.text()? != "Edit" {
            continue;
        }
        // 仅换掉默认 Edit 的撤销和重做，保留原生剪切、复制、粘贴等菜单项。
        let undo = MenuItem::with_id(app, UNDO_ID, "Undo", true, Some("Cmd+Z"))?;
        let redo = MenuItem::with_id(app, REDO_ID, "Redo", true, Some("Cmd+Shift+Z"))?;
        edit.remove_at(1)?;
        edit.remove_at(0)?;
        edit.insert_items(&[&undo, &redo], 0)?;
        return Ok(menu);
    }
    tracing::warn!("默认菜单未找到 Edit，保留原生编辑菜单");
    Ok(menu)
}

pub fn handle(app: &AppHandle<Wry>, event: MenuEvent) {
    let action = match event.id().as_ref() {
        UNDO_ID => "undo",
        REDO_ID => "redo",
        _ => return,
    };
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    // 只有 Monaco 接管时才跳过原生响应链；普通输入框继续走 AppKit 的撤销栈。
    let script = format!(
        "window.dispatchEvent(new CustomEvent('toiler:editor-history', \
         {{ cancelable: true, detail: '{action}' }}));"
    );
    let target = window.clone();
    if let Err(error) = window.eval_with_callback(script, move |unhandled| {
        if unhandled != "true" {
            return;
        }
        let target = target.clone();
        let _ = window_fallback(target, action);
    }) {
        tracing::warn!("转发编辑菜单命令失败：{error}");
    }
}

fn window_fallback(window: tauri::WebviewWindow<Wry>, action: &str) -> tauri::Result<()> {
    let target = window.clone();
    let selector = if action == "undo" {
        sel!(undo:)
    } else {
        sel!(redo:)
    };
    window.run_on_main_thread(move || {
        if !target.is_focused().unwrap_or(false) {
            return;
        }
        if let Some(main) = MainThreadMarker::new() {
            // 与 Tauri 默认菜单相同的 action 和 nil target，让 AppKit 自己找第一响应者。
            unsafe {
                NSApplication::sharedApplication(main).sendAction_to_from(selector, None, None)
            };
        }
    })
}
