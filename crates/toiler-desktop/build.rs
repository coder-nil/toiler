fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "hide_startup_overlay",
            "open_release_url",
            "open_external_url",
            "terminal_start",
            "terminal_write",
            "terminal_resize",
            "terminal_close",
        ]),
    ))
    .expect("生成桌面命令权限失败");
}
