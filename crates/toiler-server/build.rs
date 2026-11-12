//! 把前端产物目录解析成绝对路径并交给 `include_dir!`。
//!
//! 顺序：
//! 1. `WORKMAN_FRONTEND_DIR`（构建时指定，例如 pi 的静态导出产物）
//! 2. crate 内置的 `frontend/`（默认占位页，保证 `cargo build` 永远能过）
//!
//! 产物变了要重新编译（否则嵌进二进制的是旧前端），所以这里把目录登记为
//! `rerun-if-changed`。

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=WORKMAN_FRONTEND_DIR");

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let (dir, source) = match std::env::var("WORKMAN_FRONTEND_DIR") {
        Ok(raw) if !raw.trim().is_empty() => {
            let raw = raw.trim();
            let path = if Path::new(raw).is_absolute() {
                PathBuf::from(raw)
            } else {
                manifest.join(raw)
            };
            (path, "WORKMAN_FRONTEND_DIR")
        }
        _ => (manifest.join("frontend"), "builtin"),
    };

    let dir = dir
        .canonicalize()
        .unwrap_or_else(|_| panic!("前端产物目录不存在：{}", dir.display()));
    assert!(
        dir.join("index.html").is_file(),
        "前端产物目录 {} 缺少 index.html",
        dir.display()
    );

    println!("cargo:rerun-if-changed={}", dir.display());
    println!("cargo:rustc-env=WORKMAN_FRONTEND_DIR={}", dir.display());
    println!("cargo:rustc-env=WORKMAN_FRONTEND_SOURCE={source}");
}
