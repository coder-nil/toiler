# Git 面板版本升级

桌面应用的 Git 面板在“创建标签”后提供“补丁版本”“次要版本”“主要版本”。
它们是完整的项目版本升级，而不是只填写标签名。

## 升级规则与流程

以项目 `crates/toiler-desktop/tauri.conf.json` 的版本为基数，与
`tools/bump-version.py` 共用同一套代码：

| 当前版本 | 补丁版本 | 次要版本 | 主要版本 |
|---|---|---|---|
| `1.2.3` | `1.2.4` | `1.3.0` | `2.0.0` |
| `1.2.3-beta.1` | `1.2.3` | `1.3.0` | `2.0.0` |

1. 点击升级级别，先只读预览新版本、涉及的文件，以及最近可达发布标签到 HEAD 的全部提交变更记录。
2. 确认后同步 Cargo、Tauri、npm、锁文件及 Markdown/JSON 两份变更记录。
3. 只提交约定的版本文件，创建 `chore(release): 版本号升级至 <版本>` 提交。
4. 在该提交上创建普通本地标签 `v<版本>`，不会签名，不会自动推送、构建或重启应用。

预览绑定 HEAD、目标版本与内容摘要。确认前仓库发生变化时要求重新预览。
确认卡会遮住其它 Git 操作，执行期间与 Git 面板其它写操作共用仓库锁。

## 前置条件与安全边界

- 升级逻辑已用 Rust 实现（`crates/toiler-server/src/version_bump.rs`），**不需要本机
  安装 Python**。`tools/bump-version.py` 只给开发者当命令行用。
- 路由是 `POST /api/version-upgrade`，只在**跑在桌面壳里的进程**可用：环境里需要有
  `PI_WEB_DESKTOP_API_ORIGIN`（只有 Tauri 壳会注入）。命令行直启的
  `toiler-server` 没有它，直接返回 403。
- 界面上三个按钮只在桌面窗口出现（`window.__PI_WEB_DESKTOP__`），手机端与 `/m`
  手机页不渲染它们。
- **服务端不做「手机 / 隧道」硬隔离。** 早期实现曾要求请求 `Origin` 等于
  `tauri://localhost`，那条判定是错的：桌面窗口加载的是 `http://127.0.0.1:<port>`，
  页面 origin 就是回环地址，于是功能在它唯一允许的地方也恒为 403。而按回环
  origin 判定又区分不出手机与隧道 —— `lan_proxy::rewrite_request_head()` 会把它们
  的 `Origin` 改写成同一个回环地址。当前取舍是不为此加额外校验：本能力靠前端只在
  桌面壳里渲染入口来收口。若要真正的硬隔离，需要给桌面壳下发一次性令牌，
  让 `initialization_script` 注入、前端带在请求头里、服务端校验。
- 项目需要包含 `tools/bump-version.py`，并与其约定的 Toiler 项目结构兼容；这不是通用的任意语言项目版本管理器。
- 执行的是 Rust 实现，不执行项目中提供的 Python 内容。
- 仓库必须位于本地分支，工作区与暂存区干净，没有未跟踪文件，也不能有进行中的合并/变基/拣选操作。
- 需要完整 Git 历史、已配置的提交者身份、已跟踪的版本文件；目标标签不能重名，版本文件不能指向仓库外。
- 提交钩子失败或检测到并发修改时停止后续步骤；已经产生的文件或提交会保留并提示，不自动回滚，也不推送。

这里的“自动提交与建标签”仅属于桌面确认后的发布流程。直接运行
`tools/bump-version.py` 的原有 CLI 行为不变：升级文件，但不提交、不建标签。

## 验证

```bash
# Rust 侧的契约与安全边界
cargo test -p toiler-server --test version_upgrade_contract
# 开发者 CLI 用的 Python 脚本回归
python3 -B -m unittest discover -s tools -p 'test_bump_version.py'
```

契约测试里 `desktop_window_request_shape_is_allowed` 是回归用例：它发出的就是桌面
窗口真实的回环 Origin，不带任何伪造的 `tauri://` 值。早先的测试恰好用手工构造的
`Origin: tauri://localhost` 去断言「桌面壳放行」，于是把「桌面窗口恒 403」这个缺陷
盖住了。

Python 测试在项目内创建并自动清理隔离仓库，覆盖预览、三档升级、普通标签、过期确认、
未提交改动、提交失败及提交记录的完整性，不升级真实项目，也不操作真实发布标签。
