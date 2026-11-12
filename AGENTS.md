# AGENTS.md

给在本仓库里工作的 AI agent / 新人的上手说明。**先读这一页，再读
[README.md](./README.md) 与 [PLAN.md](./PLAN.md)**。

一句话定位：Toiler 是**用 Rust 重写的 pi-desktop**——同一套前端产物、同一套
`/api` 契约、同一份 v3 会话文件，但后端不再是 Node 子进程，而是一个 Tauri 单进程
（axum 跑在 Tauri 的 tokio runtime 上，前端 `include_dir!` 嵌进二进制）。

## 三条不可违反的铁律

1. **`rpi` 是 crates.io 上的依赖，不改源码。**
   `rpi-ai` / `rpi-agent` / `rpi-tools` / `rpi-harness` 的版本只写在根
   `Cargo.toml` 的 `[workspace.dependencies]`。**禁止** `[patch]`、禁止 `path`
   依赖、禁止往本地 vendor 目录里改代码。上游 API 变了就让编译器报错，然后改
   调用方。

2. **`docs/contracts/README.md` 是 `/api` 契约的事实来源。**
   它是从 pi-desktop 的 `.next` 编译产物逐字逆向出来的（工具：
   `tools/pi-contract/extract.py`）。**PLAN.md 与它冲突时以它为准**（历史上就
   有过：PLAN 写「Basic Auth」，实际实现里根本没有）。改任何 route 的形状前，
   先查这份文件；如果确实是它错了，同时更新它并说明依据。

3. **v3 会话文件是 append-only，老字节一字不改。**
   `toiler-session` 只往文件尾部追加；老行永不重写。合并、重排、重写 JSON 都
   会破坏「pi 仍能打开同一个文件」这个兼容性承诺——这是整个项目的存在前提。

## 仓库结构

```
crates/
├── toiler-poc/       阶段 0 探针（v3 可读性 / provider 流式），已冻结，别往里加东西
├── toiler-session/   会话层：v3 文件读写 + SessionStorage 实现 + 命令行
├── toiler-server/    axum 服务：/api 契约、静态托管、业务模块（主力 crate）
│   └── frontend/     include_dir! 的嵌入目录（内置占位页）
└── toiler-desktop/   Tauri 壳：同进程启服务、内置终端、手机访问（LAN 反代 + 隧道）
    ├── frontend/     前端源码（Next 应用，源自 pi-desktop）
    │   └── dist/     npm run build 的产物（不入库）
    └── shell/        Tauri 的 frontendDist（窗口占位页 + 启动失败页）
docs/contracts/        /api 契约（逆向产物，事实来源）
tools/
├── pi-contract/       从 pi-desktop 产物抽契约源码（extract.py）
└── frontend/          把外部前端产物嵌进 release 二进制的脚本（可选路径）
```

`toiler-server/src/` 里值得先认识的模块：

| 模块 | 职责 |
|---|---|
| `routes/` | 路由表与 handler（`mod.rs` 是总表，42 个 route） |
| `state.rs` | `AppState`：会话根、cwd、各 Store、活会话表 |
| `paths.rs` | 目录约定 + 允许访问的根目录（词法 + realpath 两层判定） |
| `security.rs` | Host 白名单 + Origin 同源校验（**不是** Basic Auth） |
| `session_view.rs` | 会话投影：tree / 分页 / `totalActiveMs` |
| `agent_runtime.rs` | 活会话（`LiveAgent`）、事件广播、SSE |
| `gitsvc.rs` | git：status / diff / worktree / 20 个白名单动作 |
| `apisets.rs` | Coding（`apisets`）的远端模型目录：`models-store.json` 缓存 + 上游 `/v1/models` + 按 `model.api` 分发的 provider（rpi 一个 provider id 只能一种协议） |
| `builtin_providers.rs` | 写死的 provider 目录（44 家），卡片 / `modelCount` / 登录方式都靠它 |
| `assets.rs` + `boot.rs` | 静态托管、`port=0` 绑定、CLI 与桌面壳共用启动路径 |
| `mobile_view.rs` / `mobile_access.rs` | 手机端消息投影与配对地址 |
| `desktop_tools/` | 桌面自带工具（`system_time` / `task_progress` / `ask_user`），以及 `ask_user` 的 UI 往返桥（SSE `extension_ui_request` ⇄ 命令 `extension_ui_response`） |

`toiler-desktop/src/` 里需要单独知道的一个模块：

| 模块 | 职责 |
|---|---|
| `terminal.rs` | 内置终端的 PTY（`portable-pty` 0.9）：`terminal_start/write/resize/close` 四个命令 + `terminal-output` / `terminal-exit` 两个事件。**只在桌面壳里**，服务端没有起 shell 的能力（它对局域网与隧道暴露 `/api`） |

## 常用命令

```bash
# 编译与测试（工作区根目录执行）
cargo build                                   # 全工作区；debug 版内嵌的是占位前端页
cargo build --release -p toiler-server       # 纯服务
cargo test                                    # 348 个测试，应全绿且 0 warning
cargo clippy --all-targets                    # 提交前跑一遍
cargo fmt                                     # 提交前跑一遍
cargo test -p toiler-server --test regression  # 只跑前端契约回归（最常改动的那份）

# 带真实前端构建并启动（前端有改动才重建，否则秒过）
tools/run-desktop.sh

# 跑起来
./target/release/toiler-desktop                        # 桌面形态（127.0.0.1:随机端口）
./target/release/toiler-server --port 8787 --cwd <目录>  # 纯服务形态

# 会话层命令行（排查 v3 文件最有用）
cargo run -p toiler-session --bin toiler-session -- list <root>
cargo run -p toiler-session --bin toiler-session -- show <file>
cargo run -p toiler-session --bin toiler-session -- load <file>
cargo run -p toiler-session --bin toiler-session -- verify <root>

# 契约逆向（只读 pi-desktop 的安装目录，不联网）
python3 tools/pi-contract/extract.py route sessions
```

前端源码在 `crates/toiler-desktop/frontend/`（Next 应用）。完整流程一条命令：

```bash
tools/run-desktop.sh      # 前端有改动才重建 → cargo build → 重启
```

手工分步（首次需 `npm install`）：

```bash
cd crates/toiler-desktop/frontend
npm run build                   # 产物写到 dist/
cargo build -p toiler-desktop   # build.rs 自动捡 dist/
```

没跑过 `npm run build` 时 `cargo build` 仍然能过，但二进制里是
`crates/toiler-server/frontend/` 的占位页（API 全可用）。启动日志会打印实际
嵌入的目录与「已嵌入 / 占位页」，以它为准。

**产品文案已在源码里改过**，不要再在压缩产物上打补丁：品牌名（`Toiler`）、
关于面板去掉 `pi v…`、设置侧栏顺序、各配置路径文案。清单与源码位置见
`tools/sync-frontend.sh` 头部的对照表。

## 数据目录（改路径前必读）

```
~/.agents/                 ← 通用根：skills / mcp / commands / tools（多 agent 工具共用）
└── toiler/                ← toiler 专属，**不写 ~/.pi/agent**
    ├── configs/           auth.json(0600) / models.json / settings.json /
    │                      trust.json / desktop-access*.json
    ├── sessions/          v3 会话
    ├── plugins/           预留
    └── work.sqlite        项目列表 + 选中项
```

- 首次启动从 `~/.pi/agent` **复制**一份配置与会话，**旧文件保留不删**；
  播过就不再动（`paths::seed_from_pi_if_needed`）。
- 环境变量覆盖：`AGENTS_HOME` / `TOILER_HOME` / `TOILER_AGENT_DIR` /
  `TOILER_SESSIONS_ROOT` / `TOILER_MODELS` / `TOILER_FRONTEND_DIR`。
- `PI_WEB_*`（`PI_WEB_HOSTNAME`、`PI_WEB_ALLOWED_HOSTS`、
  `PI_WEB_DESKTOP_API_ORIGIN`、`PI_WEB_PASSWORD`、`PI_WEB_PORT`、
  `PI_WEB_SHELL_PID`）与 pi 同名同义，改动前先看 `security.rs` 的注释。

## 写测试的硬要求

- **绝不让测试碰用户真实数据目录。** 用 `AppState::with_stores(...)` 把所有
  Store（`work.sqlite` / `settings.json` / `trust.json` / `auth.json` /
  `models.json`）指到临时目录。这不是洁癖：阶段 5 第一次跑契约测试时，
  `POST /api/cwd/validate` 往用户真实的 `~/.agents/toiler/work.sqlite` 里
  写进了 18 条脏项目记录。
- 打 Router 用 `tower::ServiceExt::oneshot`，不起真实端口
  （见 `tests/http_contract.rs` / `tests/regression.rs` 的开头脚手架）。
- `tests/regression.rs` 的每条用例注释都标了**它对应前端的哪一处用法**。
  加用例请沿用这个习惯；删用例前先确认前端不再依赖那个行为。
- 静态站测试只断言「产物里有 `index.html`」，换前端产物不该改测试。

## 代码风格

- 注释、文档、提交信息一律**中文**；标识符、API 字段名保持英文。
- 注释写「为什么 / 与 pi 的差异」，不要复述代码。参考 `security.rs`、
  `paths.rs`、`toiler-desktop/src/lib.rs` 的写法：差异用表格列出来。
- 模块名 `snake_case`，按领域切分（`gitsvc.rs` / `docx.rs` / `mobile_view.rs`），
  不按技术层次切分。
- `serde_json::Value` 直接拼响应的 route 不少（为了与 pi 的字段逐字一致），
  改字段名要先比对 `docs/contracts/README.md`。
- 目标：`cargo build` 与 `cargo clippy` **0 warning**。提交前 `cargo fmt`。

## 提交信息

用 Conventional Commits，**正文中文、分行写清改了什么**。历史格式参考：

```
feat(server): 补齐阶段 3 剩余后端路由
- 新增 auth 模块，读写 `auth.json`（0600 权限 + 原子替换），API Key 仅返回掩码
- 活会话表改为可克隆并新增变化广播，提供运行快照与运行中 cwd 查询
- 更新 PLAN 与 README，记录阶段 3 的模块交付与验收结果
```

scope 用 crate 短名：`server` / `session` / `desktop` / `poc` / `docs` / `tools`；
纯依赖升级用 `build(deps)`。

## 常见坑

- **不要为了「能开界面」引入 Node 或子进程。** 阶段 4 的整个价值就是删掉
  `Command::new(node)`、端口轮询和 45s 启动超时。加回来等于回退阶段 4。
- **端口由内核分配（`port = 0`）**，桌面壳不会抢 8787，也没有「端口被占用」。
  需要固定端口的场景走 `toiler-server --port`。
- **`/api` 与前端同源**：静态资源挂在 `Router::fallback`，`/api/*` 是具体路由，
  不要为了托管静态页去动 fallback 的顺序，否则兜底会把 API 吞掉。
- **手机链路**（`lan_proxy.rs` / `tunnel.rs`）是从 pi 原样移植的，含踩过的坑：
  `copy_stream` 把 `EAGAIN` 当致命错误会让手机端 1.29MB 的 JS chunk 只收到
  290KB → 白屏。动这两个文件前先读文件头注释。
- **`context` 必须做 `toolCall` 归一化**，`with_binding` 不要在 clone 后 panic
  —— 这两个都是阶段 5 修掉的致命缺陷，回归用例守着它们。
- `crates/toiler-desktop/gen/` 与 `permissions/autogenerated/` 是 Tauri 构建期
  产物，已在 `.gitignore` 里，**不要提交**。`tools/pi-contract/raw/` 同理。
  前端的 `frontend/node_modules/`、`frontend/dist/`、`frontend/.next/` 也是。
- **`not_found()` 返回的响应必须真的是 404。** 取文件用的 `body_of()` 语义是
  「命中就 200」，而 `not_found()` 会优先返回产物里的 `404.html`。直接
  `return html_of("404.html")` 会让 404 页面以 **200** 发出——占位页产物里没有
  `404.html` 时完全看不出，换上真实 Next 产物才暴露。`assets.rs` 里有回归用例
  守着，改这个函数先看它。
- **前端产物换了，静态站的行为也可能变。** 占位产物只有 `index.html`，真实 Next
  产物还有 `404.html`、`_not-found/`、`*.txt`（RSC 载荷）。拿占位产物当默认前提
  写的断言（「没有 404.html 所以我走内联分支」）会在换前端时静默失效。
- **Tailwind v4 会扫整个前端目录**用来自动收集 class，所以 `frontend/` 下
  不要放无关的大目录；也只搬被引用到的文件会让 CSS 缺类（实测缺 74 个）。

## 明确不做的事

改这些之前先确认需求真的变了：

| 项 | 原因 |
|---|---|
| MCP 服务器的**加载**（不是配置） | rpi 无 MCP 客户端。面板可以读写 `mcp.json` 并探活（`/api/mcp`，见 `docs/contracts/README.md` 差异 20），但配置里的服务器不会被连上，工具也不会出现在会话里 |
| TS 扩展的**加载**（不是安装）/ 技能市场 | rpi 是 cdylib ABI，生态不兼容。插件面板可以装/卸 npm 包并展示，但包里的 TS 扩展不会被加载 |
| OAuth 登录（`qoder-cn` 例外，已实现） | 其它家当前配置不需要 |
| models.dev 定价目录 | 可选，后补 |

> 注：`PLAN.md` 里「阶段 2」标着「进行中，主体已完成」，但阶段 3–6 都是已完成；
> README 的口径（阶段 0–5 全部完成）与实际代码一致，以 README 为准。
