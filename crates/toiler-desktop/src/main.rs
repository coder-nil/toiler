// 发布版用 GUI 子系统（Windows 上不弹控制台，pi 在同一位置踩过这个坑）；
// 开发版保留控制台，方便看 tracing / panic 输出。
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

fn main() {
    toiler_desktop_lib::run();
}
