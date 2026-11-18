# 分平台标题栏材质

## 平台策略

| 平台 | 材质 | 实色回退条件 |
|---|---|---|
| macOS 11+ | `NSVisualEffectView` 的 `BehindWindow` 材质 | 原生调用失败或系统开启“降低透明度” |
| Windows 11 22H2+ | 官方 DWM `DWMSBT_TRANSIENTWINDOW`（Acrylic） | API 不支持或失败、DWM 未合成、系统关闭透明效果、开启高对比度 |
| Windows 10 / Windows 11 早期版本 | 实色 | 不调用未公开的旧版 Acrylic 接口，避免拖动卡顿和无模糊的纯透明 |
| Linux / 网页 / 手机端 | 实色 | 不猜测 X11、Wayland 或桌面合成器是否支持模糊 |

Windows 材质不使用 Mica 代替 Acrylic：Mica 偏向壁纸色调，并不等于窗口后方内容模糊。
系统仍可根据省电、远程会话等策略减少材质效果，不能把 API 成功当作视觉验收通过。

## 实现边界

- `src/titlebar.rs` 统一返回 `solid`、`vibrancy` 或 `acrylic`。
- `sync_titlebar_appearance` 等待主线程原生调用完成后再返回，而非仅确认已排队。
- `frontend/public/native-titlebar.js` 收到成功结果才添加 `toiler-native-titlebar`
  能力类；失败或返回 `solid` 就恢复实色，不让不支持的系统留下纯透明标题栏。
- 能力类与 macOS 红绿灯避让类 `pi-overlay-chrome` 分开，Windows 不继承左侧留白。
- 顶部 42 逻辑点透明，正文仍绘制不透明背景。Windows 右上角控制按钮背景也随能力类
  切换，避免留下 138px 实色块，按钮悬停和点击保持原行为。
- 主题变化、页面就绪和重新聚焦时检查系统能力；无截屏、定时采样或 PNG 模拟。
  从系统设置返回应用时可应用新的透明/高对比度选项。

### macOS

`src/native_titlebar.rs` 维护一个具名材质视图，放在 WebView 下方，不把文字和按钮作为
材质层的子视图，不改变窗口或材质视图 alpha。原生自动布局与窗口缩放事件保持其区域；
页面就绪后再次检查当前 contentView，重复更新复用同一视图。采样和刷新由 WindowServer
完成。系统开启“降低透明度”时隐藏材质，并通知前端使用实色。

### Windows

`src/windows_titlebar.rs` 使用官方 DWM、辅助功能与只读注册表接口，不启动子进程，
不修改用户系统设置。读取个性化透明开关与高对比度选项，再尝试官方 Acrylic 和深浅色。
背景扩展至原生客户区，但 WebView 只露出标题带；失败时撤销 DWM 背景并恢复普通客户区。
旧系统由 API 返回错误自然降级，不依赖可能失真的版本字符串。

## 权限与诊断

`sync_titlebar_appearance` 必须同时列入 `build.rs` 的命令清单和
`capabilities/default.json` 的 `allow-sync-titlebar-appearance` 权限。
只注册 Rust 处理函数仍会被应用 ACL 拒绝。

前端 `html[data-titlebar-material]` 记录当前确认的状态。
macOS 日志记录实际视图数量、区域、混合模式、状态、alpha、是否关联窗口、
窗口不透明标记、页面深浅色、实际原生主题和“降低透明度”状态。
正常情况下材质数量为 1、alpha 为 1、关联窗口为 true，且窗口不透明为 false。
Windows 日志记录启用 Acrylic 或回退实色的原因。

## 手动验收（不能用编译通过代替）

1. macOS 和受支持的 Windows：在另一个窗口播放明显变色的视频或动画，确认标题栏
   中的模糊色块随后方内容变化，而非只在移动时更新。检查正文和按钮仍清晰。
2. 移动、缩放、切换全屏、切换浅深色，检查顶部区域不偏移、不残留旧材质。
3. macOS 开关“降低透明度”；Windows 开关“透明效果”和高对比度。返回应用后确认
   按系统设置启用或关闭透明，不要求用户关闭其需要的辅助功能。
4. Windows 检查右上角关闭/最小化/还原、拖动及最大化，确认没有额外左侧红绿灯留白。
5. 旧 Windows、Linux、浏览器和手机端保持实色，不出现内容穿透。

测试记录应区分本机自动化、Windows 编译、跨平台 API 类型检查及各平台真机视觉验收。
