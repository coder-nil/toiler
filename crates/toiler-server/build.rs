//! 把前端产物目录解析成绝对路径并交给 `include_dir!`。
//!
//! 顺序：
//! 1. `TOILER_FRONTEND_DIR`（构建时指定，例如外部的 Next 导出产物）
//! 2. 源码树里的 `crates/toiler-desktop/frontend/dist/`（`npm run build` 的产物）
//! 3. crate 内置的 `frontend/`（默认占位页，保证 `cargo build` 永远能过）
//!
//! 第 2 条让「在本仓库里改前端 → 重新编译」成为常规路径：前端源码与 Rust 同在
//! 一个仓库，`npm run build` 把产物写进 `dist/`，这里自动捡到，不需要手动复制
//! 或设环境变量。产物目录被登记为 `rerun-if-changed`，重新构建前端会触发
//! 重新编译（否则嵌进二进制的是旧前端）。
//!
//! 注意 `include_dir!` 要求编译期常量路径，所以最终的绝对路径通过 `rustc-env`
//! 传给 `assets.rs`，而不是在宏里拼路径。

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=TOILER_FRONTEND_DIR");

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));

    // 与本 crate 同级、位于 toiler-desktop 下的前端构建产物。
    let sibling_dist = manifest
        .parent()
        .map(|crates| crates.join("toiler-desktop").join("frontend").join("dist"));

    let (dir, source) = match std::env::var("TOILER_FRONTEND_DIR") {
        Ok(raw) if !raw.trim().is_empty() => {
            let raw = raw.trim();
            let path = if Path::new(raw).is_absolute() {
                PathBuf::from(raw)
            } else {
                manifest.join(raw)
            };
            (path, "TOILER_FRONTEND_DIR")
        }
        // 只在产物完整（有 index.html）时才用 dist/，未构建前端就退回占位页，
        // 这样刚 clone 下来没跑 npm 的人依然能 `cargo build`。
        _ => match &sibling_dist {
            Some(dist) if dist.join("index.html").is_file() => (dist.clone(), "frontend-dist"),
            _ => (manifest.join("frontend"), "builtin"),
        },
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
    // 目录本身只登记一条时，Cargo 只盯目录 mtime：产物里换了文件名、而目录
    // mtime 没变的情况下，`cargo build` 会把**旧前端**继续嵌进二进制（实测过：
    // `cargo build -p toiler-desktop` 重编了桌面壳，server 二进制却还是上一版
    // 前端）。所以把产物里的每个文件都登记一遍。
    for path in files_under(&dir) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    println!("cargo:rustc-env=TOILER_FRONTEND_DIR={}", dir.display());
    println!("cargo:rustc-env=TOILER_FRONTEND_SOURCE={source}");
}

/// 递归列出目录下的常规文件（前端产物是几十~几百个小文件，不引第三方依赖）。
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(path),
                Ok(kind) if kind.is_file() => out.push(path),
                _ => {}
            }
        }
    }
    out
}
