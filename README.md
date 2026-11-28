# Toiler

Toiler 是一个桌面 AI 编程工作台。你可以在同一窗口里与模型对话、浏览项目文件、运行终端，以及审阅和提交 Git 改动。它沿用 pi-desktop 的界面基础和 v3 会话格式，后端由 Rust 重写，桌面运行不需要 Node.js。

[下载安装包](https://github.com/coder-nil/toiler/releases/latest) · [查看更新记录](./CHANGELOG.md)

## 安装与上手

从 [GitHub Releases](https://github.com/coder-nil/toiler/releases) 下载对应平台的安装包。发布页附有 `SHA256SUMS.txt`，可用于校验下载文件。

| 平台 | 安装包 | 注意事项 |
| --- | --- | --- |
| macOS（Apple Silicon / Intel） | `.dmg` | 应用采用 ad-hoc 签名，未做 Developer ID 签名和公证。确认来源后，如被 Gatekeeper 拦截，可在「系统设置 → 隐私与安全性」选择「仍要打开」。详见 [macOS 发布说明](./docs/macos-release.md)。 |
| Windows（x64） | NSIS `.exe` 或 `.msi` | 发布前进行 Defender 扫描，但安装包未做代码签名。遇到拦截请先查看保护历史记录，不要关闭防护。详见 [Windows 发布说明](./docs/windows-release.md)。 |
| Linux（x64） | `.deb` 或 `.AppImage` | 按发行版和使用习惯选择。 |

1. 打开 Toiler，在侧栏选择项目目录。如果以前用过 pi，可以在会话列表中查看已导入的历史。
2. 在「设置 → 模型」配置供应商凭据，启用所需模型，再新建对话或继续历史会话。兼容的供应商可刷新模型列表；也可以配置自定义供应商。
3. 从侧栏打开文件浏览器或 Git 工作区。Git 功能依赖本机的 `git`，内置终端只在桌面应用中提供。

## 主要功能

- **对话与项目文件**：支持流式对话、历史搜索、上下文续聊和对话导航；可在聊天中引用项目文件，在文件面板中预览文本、Markdown、图片和 DOCX。
- **Git 工作区**：集中处理暂存、提交、拉取和推送，查看文件差异、提交关系图与分支标签。历史可按提交 ID、摘要、作者和日期筛选。推送前可核对目标分支、待推送的提交和文件改动；取消推送不会撤销本地提交。
- **合并冲突**：拉取合并遇到冲突时，可在文件对比区使用三栏编辑器，左侧看本地版本、右侧看传入版本、中间编辑结果。点击「应用」才保存并暂存；全部解决后点击「继续合并」，不会自动推送。内置编辑器支持不超过 256 KiB 的 UTF-8 文本，其他文件可在终端处理。
- **技能与插件**：可从 skills.sh 搜索技能，并从 GitHub 安装。插件面板支持查找、安装、更新和卸载 npm 包，但不会加载 Pi 的 TypeScript 扩展。只有 npm 插件管理需要本机的 Node.js 和 npm，可用 `TOILER_NPM` 指定 npm 路径。
- **手机访问**：在「设置 → 手机访问」设置密码并开启局域网访问，再用手机扫码打开。需要外网访问时，可配置依赖 `cloudflared` 的 Cloudflare 隧道。密码校验由桌面的手机访问代理负责，手机端不提供内置终端。
- **桌面界面**：用 `⌘`（macOS）或 `Ctrl`（Windows / Linux）加 `+`、`-` 或 `0` 放大、缩小或还原界面。新建对话页的插画与运行中的应用图标按四立交节时刻换季，可离线显示。

Git 面板还可为兼容的项目执行补丁、次要或主要版本升级：先预览变更记录，确认后才同步版本文件、创建本地版本提交和标签，不自动推送。详见 [版本升级说明](./docs/version-upgrade.md)。

当前限制：MCP 面板可维护配置与探活，但不会加载 MCP 服务器或把工具注入会话；订阅 OAuth 仅实现 Qoder CN。兼容性细节见 [接口契约](./docs/contracts/README.md)。

## 数据与 pi 兼容性

Toiler 默认使用独立的数据目录：

```text
~/.agents/
└── toiler/
    ├── configs/       模型、凭据、偏好、信任和手机访问配置
    ├── sessions/      pi v3 JSONL 会话
    ├── plugins/       Toiler 安装的插件
    ├── workspace/     未选中有效项目时的桌面兜底目录
    └── work.sqlite    项目列表与选中项
```

- 启动时会尝试从 `~/.pi/agent` 复制配置和缺失的会话；首次创建项目库时会尝试从旧项目库导入。原文件不移动、不删除，已经复制的会话不会被覆盖，两边的修改也不会同步。
- 续聊保留 pi v3 格式，只向 Toiler 的会话文件末尾追加，不改写原有条目。追加后的文件仍可由 pi 读取。
- 桌面启动若没有可用的当前目录，会优先使用上次选中的有效项目，再回退到专用 `workspace/`，而不是把用户主目录当作兜底项目。

## 从源码运行

桌面构建需要 Rust stable、[Tauri 平台依赖](https://v2.tauri.app/start/prerequisites/) 和用于构建前端的 Node.js。本地前端开发统一使用 nvm 管理的 Node **22.23.5**；发布 CI 的各平台依赖见 [发布工作流](./.github/workflows/release.yml)。

以下命令从仓库根目录执行（macOS / Linux 的 Bash）：

```bash
source "${NVM_DIR:-$HOME/.nvm}/nvm.sh" &&
nvm use 22.23.5 &&
node --version &&
(cd crates/toiler-desktop/frontend && npm ci) &&
tools/run-desktop.sh
```

脚本按需重建前端、编译桌面应用，然后重启同路径下的旧实例。可用 `--rebuild-frontend` 强制重建、`--skip-frontend` 复用已有产物、`--no-build` 只重启，或 `--foreground` 直接查看终端日志。

只构建、不启动时，在同一 Node 环境下执行：

```bash
(cd crates/toiler-desktop/frontend && npm run build) &&
cargo build --release -p toiler-desktop
```

前端静态产物位于 `crates/toiler-desktop/frontend/dist/`，会由 `toiler-server` 的构建脚本自动嵌入二进制。未构建前端时，直接用 Cargo 编译得到的程序只显示占位页，API 仍可用；`tools/run-desktop.sh` 在编译时要求有实际前端产物。

### 独立运行 HTTP 服务

不需要桌面窗口时，可运行同一个 Rust 服务。若需要浏览器界面，先按上面的步骤生成前端产物。

```bash
cargo build --release -p toiler-server &&
./target/release/toiler-server --port 8787 --cwd /path/to/project
```

打开 `http://127.0.0.1:8787/`。桌面应用则默认使用 `127.0.0.1` 上的随机端口。

> 独立服务的 Host / Origin 校验不等于用户认证。直接改成 `--host 0.0.0.0` 不会启用桌面的访问密码。建议保持回环监听；若必须远程部署，请在受控反向代理上配置认证和 TLS，不要直接暴露服务端口。

## 开发与维护

| 目录 | 职责 |
| --- | --- |
| [`crates/toiler-desktop`](./crates/toiler-desktop) | Tauri 桌面应用、Next.js 前端、内置终端和手机访问代理 |
| [`crates/toiler-server`](./crates/toiler-server) | axum HTTP/SSE 服务、模型配置、Git、文件与静态资源托管 |
| [`crates/toiler-session`](./crates/toiler-session) | pi v3 会话读写和诊断命令行 |
| [`crates/toiler-poc`](./crates/toiler-poc) | 保留的早期兼容性探针，不参与产品运行 |

桌面端在 Tauri 进程内运行 axum，界面和 `/api` 同源。Agent 运行时使用 crates.io 上未修改的 `rpi`；实际版本以 [`Cargo.lock`](./Cargo.lock) 为准。升级依赖时统一修改根 [`Cargo.toml`](./Cargo.toml) 的 `[workspace.dependencies]`，再执行：

```bash
cargo update -p rpi-ai -p rpi-agent -p rpi-tools -p rpi-harness
```

- 构建与验证规范：[AGENTS.md](./AGENTS.md)、[前端说明](./crates/toiler-desktop/frontend/README.md)。涉及前端的验证应先生成真实产物，再运行 Rust 构建与测试。
- 修改 `/api` 前先查阅 [接口契约](./docs/contracts/README.md)；历史实施与验收记录见 [PLAN.md](./PLAN.md)，版本变化见 [CHANGELOG.md](./CHANGELOG.md)。
