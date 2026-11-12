# Toiler

基于 [pi-rust (`rpi`)](https://crates.io/crates/rpi-harness) 的桌面编程工作台。
`rpi` 作为**未修改的 crates.io 依赖**引入，升级只需改版本号。

当前锁定 **rpi 0.21.2**（2026-11-09 发布）。从 0.6.2 升级到 0.21.2 零代码改动，
四项验证在 0.21.2 下重新跑通。

当前状态：**阶段 0–5 全部完成**（PoC → v3 会话层 → HTTP 服务 → 全部 route →
Tauri 单进程 → 回归）。四项关键验证全部通过；560 个真实 pi 会话全量可读、
可原地续聊，且 pi 自己能复读 toiler 追加过的文件。

## 四项验证结论

| # | 验证项 | 结果 |
|---|---|---|
| 1 | crates.io 上的 rpi 能被独立项目引入编译 | ✅ `rpi-ai 0.21.2` / `rpi-harness 0.21.2` |
| 2 | `openai-responses` 对自定义 baseUrl 真实流式出字 | ✅ `coding.apisets.com` + `gpt-6-astra` |
| 3 | pi 的 `models.json` 可直接映射为 rpi 的 `Model` | ✅ 无需改写配置 |
| 4 | v3 会话文件可被 rpi 读取 | ✅ 99,132 / 99,132 条消息直接通过 |

## 升级方式

```toml
# Cargo.toml
rpi-ai = "0.21"        # ← 只改这里
rpi-harness = "0.21"
```

```bash
cargo update -p rpi-harness   # 拉新版本
cargo build                   # 若 API 变了，编译器直接告诉你
```

不改 rpi 源码，不用 `[patch]`，不用 path 依赖。

## 运行

两种形态，用同一个 axum 服务与同一份配置。

### 桌面应用（正式形态）

```bash
# 首次：装前端依赖
tools/run-desktop.sh     # 会提示先装；或手动：
                         # cd crates/toiler-desktop/frontend && npm install

# 日常：前端有改动才重建，然后编译 + 启动（一条命令搞定）
tools/run-desktop.sh
```

`tools/run-desktop.sh` 做的事：前端源码比 `dist/` 新才重跑 `npm run build`
（否则秒过）→ `cargo build --release` → 停掉旧实例 → 启动 → 打印实际端口。

| 参数 | 用途 |
|---|---|
| `--rebuild-frontend` | 强制重建前端（改了 `next.config.ts` 或依赖时） |
| `--skip-frontend` | 完全不碰前端，只编译 Rust（调后端时最快） |
| `--no-build` | 只重启，不编译（二进制已是最新时） |
| `--foreground` / `-f` | 前台运行，日志直接打终端 |

手工分步也可以：

```bash
cd crates/toiler-desktop/frontend && npm run build   # 产物到 dist/
cargo build --release -p toiler-desktop              # build.rs 自动捡 dist/
./target/release/toiler-desktop
```

前端源码就在 `crates/toiler-desktop/frontend/`（来自 pi-desktop 的 Next 应用，
已去掉 Node 后端部分）。产物由 `crates/toiler-server/build.rs` 自动发现，不需要
手动复制或设环境变量。

从 Finder 双击也行（`bundle` 见 `crates/toiler-desktop/tauri.conf.json`）。
启动后只监听 `127.0.0.1:<随机端口>`，手机访问要另外在设置页开关。

### 纯服务（开发 / 远程访问）

```bash
cargo build --release -p toiler-server
./target/release/toiler-server --port 8787 --cwd /path/to/project
```

打开 `http://127.0.0.1:8787/`。

> 前端产物是否存在决定二进制里是「真界面」还是「占位页」：
> 跑过 `crates/toiler-desktop/frontend` 里的 `npm run build` 就有界面，
> 否则 `cargo build` 仍然能过，但内嵌的是 `crates/toiler-server/frontend/`
> 的占位页（API 全可用）。启动日志会打印实际嵌入的产物目录与
> `已嵌入 / 占位页`，以它为准。

参数：

| 参数 | 默认 | 说明 |
|---|---|---|
| `--host` | `127.0.0.1` | 监听地址；改 `0.0.0.0` 会对局域网开放（需自行设密码） |
| `--port` | `8787` | 端口；`0` = 内核分配 |
| `--sessions-root` | `~/.agents/toiler/sessions` | 会话根 |
| `--cwd` | 进程当前目录 | 默认项目目录 |

数据位置（详见 `docs/contracts/README.md`）：

```
~/.agents/toiler/
├── configs/      auth.json / models.json / settings.json / trust.json / desktop-access*
├── sessions/     会话
└── work.sqlite   项目列表
```

首次启动会自动从 `~/.pi/agent` 复制一份配置与会话（旧文件保留）。

### 诊断工具

```bash
cargo run -p toiler-poc --bin toiler-poc -- info     # 只做 models.json → rpi 的映射
cargo run -p toiler-poc --bin v3probe                # 扫描 pi 会话的 v3 兼容性
```

## 关键发现

### 1. provider 差距是假问题

pi 有 93 个 provider，rpi 只有 4 个（anthropic / openai-completions /
openai-responses / faux）。但实际配置里两个 provider 都是标准
`openai-responses`：

```json
"OpenAI":   { "api": "openai-responses", "baseUrl": "https://coding.apisets.com/v1" }
"pleasure": { "api": "openai-responses", "baseUrl": "https://api.pleasure2ai.top/v1" }
```

`rpi-cli` 的 `provider_to_models()` 直接认这三个 api 值，字段名
（`baseUrl`/`apiKey`/`contextWindow`/`maxTokens`/`reasoning`/`input`）全对得上。

### 2. 会话 v3 兼容性比预期好得多

- **543/544** 个文件是 v3 header（1 个是早期无 header 的裸条目文件，可跳过）
- **101,359** 条条目，时间戳 100% 可解析（ISO → unix ms 需要自己转）
- **99,138 / 99,138** 条 message 直接反序列化成 `rpi_ai::Message`

只有两处需要适配层归一：

| 差异 | 影响 | 处理 |
|---|---|---|
| `stopReason: "toolUse"` vs rpi 要 `"tooluse"` | 40,799 条 assistant | 字段归一，一行代码 |
| `bashExecution` 缺 `content` 字段 | 6 条 | `output` 转成文本块，映射成 `CustomMessage` |

**注意 rpi 有两层消息类型**，别混淆：

```
rpi_ai::Message         tag = "role"   ← pi 的线格式，v3 直接兼容 ✅
rpi_agent::AgentMessage tag = "kind"   ← rpi 自己的持久化格式 ❌
```

v3 文件要映射到 `rpi_ai::Message`，绕开 `AgentMessage`。

### 3. v3 条目缺 `seq`

v3 条目没有 `seq` 字段，而 rpi 的 `EntryBase` 要求它（`SessionState` 有
连续 seq 不变量）。自定义 `SessionStorage` 实现时按行号补即可。

### 4. rpi 的 jsonl 层不能直接用

`rpi-harness` 的 `JsonlSessionRepo` / `JsonlSessionStorage` 只认 v4：

```rust
// session/jsonl/codec.rs:119（0.21.2）
if version != 4 {
    return Err(schema("has unsupported session version"));
}
```

**正解**：实现 `rpi_harness::session::types::SessionStorage`
（`pub trait SessionStorage: Send + Sync`，纯 async trait，公开），
自己解析 v3 并继续 append v3。绕开 jsonl 层，零文件改动。

### 5. 首字延迟是端点特性，不是 rpi 的问题

`gpt-6-astra` 开启 reasoning 后，思考 tokens 走完才出第一个文本 delta，延迟随
端点负载波动（曾测到 37~71 秒，0.21.2 复测为 4.6 秒）。同一端点用 pi 走也一样。

## 已知缺口（rpi 侧没有，需要自己补或明确砍掉）

| 能力 | pi | rpi | 处理 |
|---|---|---|---|
| MCP | ✅ 内置 | ❌ | 自己实现，或砍掉 |
| TS 扩展 / 插件市场 | ✅ | ❌（只有 Rust cdylib ABI） | 砍掉 |
| models.dev 定价目录 | ✅ | ❌ | 砍掉 |
| 供应商目录（44 家 provider） | ✅ provider 目录 | ❌ | 写死在 server 端：`builtin_providers.rs` |
| OAuth 登录 | ✅ | 仅扩展内 | 只有 `qoder-cn` 自己实现了（见 `qoder_cn/`） |
| 内置工具 | read/write/edit/bash/grep/find/ls… | 同左 + powershell | ✅ 够用 |

## 阶段 1：v3 会话层（`crates/toiler-session`）

目标：不迁移、不改格式，**直接读写 pi 的 v3 `.jsonl`**，让 pi 与 toiler 并行打开同一份历史。

```bash
cargo run -p toiler-session --bin toiler-session -- init      # 初始化 ~/.toiler/agent/sessions
cargo run -p toiler-session --bin toiler-session -- list       # 列会话
cargo run -p toiler-session --bin toiler-session -- load <file>
cargo run -p toiler-session --bin toiler-session -- append <file> "文本"   # 追加一轮
cargo run -p toiler-session --bin toiler-session -- verify ~/.pi/agent/sessions  # 全量回归
```

### 实现骨架：委托 + 写穿

`SessionStorage` 有 30+ 个方法（entries / records / lanes / progress / 分页 / 统计）。
手写一遍既慢又容易和 rpi 语义跑偏，所以：

- **读**：v3 文件逐行回放成 `SessionState`，灌进 rpi 自己的
  `InMemorySessionStorage::from_state`。所有读方法直接转发，分支查询、分页、
  统计全部由 rpi 原生实现负责。
- **写**：`append_entry` 先落内存（拿到 rpi 盖好 `seq`/`parentId` 的 `Entry`），
  再按 v3 格式 append 一行到文件尾。**老行永不重写**，所以 pi 仍能打开同一文件，
  也不会因为字段丢失（rpi 没有 `durationMs` 之类）而破坏历史。

### 回放时补的三处 v3 缺口

| 缺口 | 处理 |
|---|---|
| v3 条目没有 `seq` | 按**成功落地**的顺序递增；失败的行不推进，避免「一条坏行 → 整串非连续」的级联 |
| v3 是 `parentId` 链，rewind/edit 会让 `parentId` 指向非叶子 | rpi 要求 `parentId == lane 叶子`，所以先把 main 叶子退回到 parent 再 append |
| `stopReason: "toolUse"` vs rpi 要 `"tooluse"` | 读时小写、写时还原 camelCase |

### 又发现两处 pi 侧约束

1. **pi 的自定义消息 role 必须是 `"custom"`**（`dist/core/messages.js` 的
   `convertToLlm` 只认 `bashExecution|branchSummary|compactionSummary|custom|system`）。
   不在这个集合里的 role，写进 v3 时降级成 `role:"custom"` + `customType`。
2. **pi 的 v3 加载是宽松的**：`JSON.parse` 失败只跳过该行。所以「pi 能读」很容易，
   真正要保证的是 `buildSessionContext` 能把追加的消息纳入上下文——已验证。

### 验收证据

| 项 | 结果 |
|---|---|
| 554 个真实 pi 会话全量回放 | 554/554，102,572 条目，0 行跳过 |
| 复制真实会话到 `~/.toiler` | 可列出 / 可加载 / 可追加 |
| 追加后老字节 | 完全一致（append-only） |
| pi 侧复读追加后的文件 | `loadEntriesFromFile` 270 条全解析；`buildSessionContext` 纳入追加的两条 |

## 阶段 2：HTTP/SSE 服务（`crates/toiler-server`）

把 pi-desktop 的 `/api` 契约用 axum 复刻。**契约从 `.next` 编译产物逐字逆向**，
结论固化在 [`docs/contracts/README.md`](./docs/contracts/README.md)，逆向工具在
`tools/pi-contract/extract.py`（可重新抽任意 route / 共享模块）。

```bash
cargo run -p toiler-server -- --port 8787          # 会话根默认 ~/.pi/agent/sessions
cargo run -p toiler-server --bin catalog-probe     # 只看 models.json → provider 映射
```

### 关键事实（与直觉相反的两处）

1. **没有 Basic Auth**。pi-desktop 用的是「Host 白名单 + Origin 同源」校验
   （`lib/request-security.ts`）：Host 必须是 localhost / 任意 IP / 白名单主机，
   且 Origin 同源或缺失。我们 1:1 复刻，含 `export` 的 navigate 例外。
2. **pi 的 v3 读取是宽松的**，所以「能读」不难；难的是 `tree`/`paging`/`totalActiveMs`
   这些前端形状 —— 都是 `.next` 里的压缩函数，逐个复刻（见 `session_view.rs`，
   每个函数注释标了产物模块号）。

### agent 运行时

```
POST /api/agent/{id} {type:"prompt"} 
  → AgentHarness::create(allow_existing_session: true, models = models.json 的 providers)
  → lane.send_message(user message)          ← 真实调用 provider
  → AgentEvent 投影成 pi 的 SSE 形状 → broadcast → GET /events
  → 消息按 v3 写进同一个 .jsonl             ← 阶段 1 的写穿
```

默认模型取「上次用的模型」（最近会话里最后一个 `model_change`），可用
`TOILER_MODEL=provider/id` 覆盖。

### 已验收

| 项 | 结果 |
|---|---|
| 555 个历史会话 | 可列 / 可读，字段与 pi 对齐 |
| 真实一轮对话 | `agent_start → message_start → message_update×18 → tool_execution_* → agent_end` |
| 工具调用 | `ls` 真的执行，toolResult 入档 |
| 多轮续聊 | 第二轮正确引用第一轮上下文 |
| 写出的 v3 文件 | pi 的 `loadEntriesFromFile` + `buildSessionContext` 均正常 |
| 测试 | 阶段 2 结束时 72 个（安全 6 + 视图 12 + 列表 2 + 搜索 5 + HTTP 契约 10 + 会话层 …） |

## 阶段 3：剩余 route（auth / models-config / git / projects）

`docs/contracts/README.md` 的「实现进度」表已全量更新。要点：

- **配置自己的目录**：`~/.agents/` 放跨工具通用的东西（`skills/`/`mcp/`/
  `commands/`/`tools/`），`~/.agents/toiler/` 放 toiler 专属的
  `configs/`（auth/models/settings/trust/desktop-access）、`sessions/`、
  `plugins/` 与 `work.sqlite`。首次启动从 `~/.pi/agent` **复制**一份过来
  （旧文件保留不删），之后各管各的。
- **项目库独立**：`~/.agents/toiler/work.sqlite`（`added_projects` /
  `app_preferences`）；首次创建时从旧的 `~/.pi/agent/pi.sqlite` **只读**
  导入一次，之后两边各自独立演进。
- **git 走 CLI**：20 个动作、worktree、diff 全部包在 `gitsvc.rs`，每个仓库一把锁；
  凭据走临时 `GIT_ASKPASS`（不落明文到命令行）。
- **真模型的三处调用**：`auto-name`（起标题）、`git/commit-message`（写提交信息）、
  `models-config/test`（连通性自检）都复用 `oneshot.rs` 的一次性补全。
- **`projectRoot` 修好了**：改成真 `git rev-parse` 探测，worktree 会话会带
  `worktreeBranch`（阶段 2 的已知差异 1 已消）。

```bash
cargo test                      # 101 个测试
cargo run -p toiler-server -- --port 8787
curl -s "localhost:8787/api/git?cwd=$PWD" | jq .
curl -s "localhost:8787/api/auth/providers" | jq .
```

### 仍未做

`files?type=diff` 与图片缩略、手机端消息二次裁剪、模型价格目录（models.dev 已按
计划砍掉）、除 `qoder-cn` 之外的 OAuth 登录（返回 501）。前端联调已在阶段 4 打通（见下）。

`qoder-cn`（Qoder 国内版订阅）是完整移植：设备码登录、COSY 签名、请求体改写、
SSE 信封解包、15 个内置模型 —— 见 `crates/toiler-server/src/qoder_cn/`。
rpi 的 provider 不允许注入传输层，所以模型的 `baseUrl` 指向本进程的
`/qoder-cn/<token>/v1/chat/completions`，由它完成改写（等价 pi 的 `withQoderCnFetch`）。

供应商目录（pi 面板里那 44 家）已经补齐：写死在
`crates/toiler-server/src/builtin_providers.rs`（pi SDK 目录 42 家 + pi-desktop
自注册的 `apisets` / `qoder-cn`），由 `/api/auth/providers`（10 家订阅）
与 `/api/auth/all-providers`（42 家 API Key）输出。但**内置模型目录仍没带过来**：
填了 API Key 之后，模型 id 还得自己写进 `models.json`。

## 阶段 4：Tauri 单进程（`crates/toiler-desktop`）

一个可执行文件里同时跑壳、后端与前端：**没有 node，没有子进程**。

```bash
# 不嵌真实前端（二进制里是占位页，API 照常可用）
cargo run -p toiler-desktop

# 嵌前端源码构建出的产物（在 frontend/ 里先跑 npm run build）
cd crates/toiler-desktop/frontend && npm run build && cd -
cargo build -p toiler-desktop --release
./target/release/toiler-desktop
```

### 结构

| 模块 | 作用 |
|---|---|
| `toiler_server::boot` | 启动引导：会话根 → 模型目录 → `AppState` → `bind` / `serve`；CLI 与桌面壳共用 |
| `toiler_server::assets` | `include_dir!` 嵌前端；`_next/static` immutable、目录 index、RSC `.txt`、客户端路由兜底 |
| `toiler-desktop::server` | 把 `bind(127.0.0.1:0)` → `tauri::async_runtime::spawn(serve)`，顺手预热会话列表 |
| `toiler-desktop::frontend` | **前端源码**（Next 应用，源自 pi-desktop）；`npm run build` 产出 `dist/` |
| `toiler-desktop::shell` | Tauri 的资源目录（`frontendDist`）：只有窗口占位页与启动失败页 |
| `toiler-desktop::terminal` | 内置终端的 PTY（`portable-pty`）：四个 Tauri 命令 + 两个事件，见下面的「内置终端」 |
| `toiler-desktop::{lan_proxy,tunnel}` | 从 pi 原样移植的手机访问件（局域网反代 + Cloudflare 隧道） |

三处与 pi 的关键差别：

1. **同源**：窗口直接加载 `http://127.0.0.1:<port>/`（pi 是 `index.html` + 另一个
   API origin）。`/api` 与前端一个端口，SSE/fetch 不用处理跨域。
2. **端口由内核分配**（`port = 0`）：不再抢 8787，也不会出现「端口被占用」。
3. **没有启动探测**：`bind` 完成即可用（pi 是轮询 45s 等 node 起来）。

### 内置终端

`terminal.rs` 给前端的 `TerminalPanel`（xterm）提供真 PTY，与 pi 同版本依赖、
同一套协议：

| 方向 | 接口 |
|---|---|
| 前端 → 后端 | `terminal_start({cwd, cols, rows}) → terminalId`、`terminal_write({terminalId, data: number[]})`、`terminal_resize({terminalId, cols, rows})`、`terminal_close({terminalId})` |
| 后端 → 前端 | `terminal-output`（`{terminalId, data: number[]}`）、`terminal-exit`（`{terminalId}`） |

几个刻意的选择（改之前先看 `terminal.rs` 的头注释与测试）：

- **只在桌面壳里**：它开的是任意 shell；服务端要对局域网和隧道暴露 `/api`，
  所以起进程的能力不能放在那边。手机链路天然没有这条路径。
- **cwd 必须是绝对路径且真实存在**：从访达启动时进程 cwd 是 `/`，相对路径会解析
  到 `Toiler.app/Contents/MacOS/` 里。
- **继承的 locale 不是 UTF-8 就补 `LC_ALL`/`LC_CTYPE`**：GUI 启动没有 `LANG`，
  zsh 会把中文输入法送来的字节按单字节回显（「。」变成 `\M-ã<0080><0082>`）。
- **关面板 = 杀进程组**：只杀 shell 会留下 `npm run dev` 之类的子进程。

### 前端

前端源码在 `crates/toiler-desktop/frontend/`，是从 pi-desktop 搬过来完整 Next
应用（排除 Node 后端部分）。三个目录各管一件事，不要混：

| 路径 | 内容 | 进 git |
|---|---|---|
| `crates/toiler-desktop/frontend/` | Next 源码（`app/` `components/` `hooks/` `lib/` `data/` `public/`） | 是 |
| `crates/toiler-desktop/frontend/dist/` | `npm run build` 的静态导出产物 | 否 |
| `crates/toiler-server/frontend/` | `include_dir!` 的嵌入目录（占位页） | 是 |
| `crates/toiler-desktop/shell/` | Tauri 的 `frontendDist`（窗口占位页 + 启动失败页） | 是 |

`build.rs` 选目录的顺序：`TOILER_FRONTEND_DIR` → `frontend/dist/`（存在
`index.html` 时）→ `crates/toiler-server/frontend/` 占位页。所以日常开发只用：

```bash
cd crates/toiler-desktop/frontend && npm install   # 首次
npm run build                                       # 产出 dist/
cargo build -p toiler-desktop                       # 自动嵌入 dist/
```

与 pi 的前端差异（都是为了去掉 Node 后端）：

| 项 | pi | toiler |
|---|---|---|
| `output` | 按目标在 `standalone` / `export` 间切 | 恒为 `export` |
| `distDir` | `.next` / `.next-desktop-dev` / `.next-desktop-frontend` | 恒为 `dist` |
| `app/api/` | pi 的 Node 后端共 40+ route | **不搬**，`/api` 由 axum 提供 |
| `app/m/` | 手机页面（`force-dynamic`） | **不搬**，`output:export` 会因预渲染失败 |
| `proxy.ts` / `serverExternalPackages` / `headers()` | pi 的 server 侧特性 | 删掉；安全头改由 `assets.rs` 发 |

> `lib/` 里有少数文件 import 了 node 内置模块（`path` / `fs` / `node:module`）。
> 它们本来是 pi 的后端工具，但 `lib/paths.ts` 的 `samePath` 同时被浏览器端的
> `FileExplorer` 用。webpack 会把 `path` polyfill 成前端实现，已实测产物正常；
> 改动这几个文件时注意不要引入只有 Node 能跑的 API。

### 验收

| 项 | 结果 |
|---|---|
| 冷启动（exec → 首页 200） | 中位 **114ms** |
| 二进制 | 51MB（含 24MB 前端）；pi 的 `.app` 是 255MB |
| 前端渲染 | 真实 pi 产物内嵌后界面完整：会话列表 / 项目切换 / 模型选择 / 文件面板（含 git 状态） |
| `/api/sessions` | 559 个会话；首次扫描 7.8s → **1.68s**（并行），预热后 13ms |
| 手机访问 | LAN 代理 200（Basic Auth）/ 无凭据 401；隧道起停正常，退出不留孤儿 |
| 测试 | 151 个全绿 |

## 阶段 5：回归（258 个测试全绿）

一个阶段的价值不在「写了多少」，而在「走查时抓到多少测试抓不到的问题」。
阶段 5 用真实前端产物 + 真实模型 + 真实手机链路走了一遍，修掉 6 个致命缺陷
（详见 [PLAN.md](./PLAN.md) 阶段 5）：

| 缺陷 | 症状 |
|---|---|
| `context` 不做 `toolCall` 归一化 | **打开任何带工具调用的历史会话整页白屏** |
| `with_binding` 在 clone 后 panic | **应用一启动就 Abort trap** |
| `copy_stream` 把 `EAGAIN` 当致命错误 | **手机端 1.29MB 的 JS chunk 只收到 290KB，白屏** |
| 壳没注入 `PI_WEB_*` 环境变量 | 配对地址永远停在 `starting` |
| `LiveAgent` 忽略 `settings.json` 默认模型 | 续聊用错模型 |
| `/api/mobile/state` 直接下发原始消息 | 一屏 474KB（图片 base64 占 682KB）→ 54KB |

主要补齐：

- **`/api/files` 全量重写**：六种 `type`（`list`/`read`/`download`/`meta`/`preview`/`watch`）
  + 三种写方法（`PUT`/`PATCH`/`POST` multipart），`Range` 206/416、SSE 命名事件、
  两层授权（词法 + realpath）+ `sessionId` 引用通道
- **DOCX 预览**：自己解 OOXML（无 Node 依赖），标题/粗斜体/列表/表格
- **手机访问接口**：`/api/mobile/access`（读写配置）+ `/api/mobile/pair`（地址推导，不含凭据）
- **手机消息投影**（`mobile_view`）：图片转占位、思考/工具入参截断、30 条窗口 + 40 条内保留细节

验收（真实数据）：

| 项 | 结果 |
|---|---|
| 测试 | 258 个全绿，0 warning |
| 历史会话 | 560/560 可读（104,986 条目 / 103,318 消息，0 失败） |
| 真实续聊 | 老字节一字不改；pi 自己的 `SessionManager` 复读得 388 条目 / 385 context 消息 |
| 桌面界面 | 会话/文件/图片/语法高亮/Markdown 预览/手机设置页全部实测 |
| 手机 | Basic Auth 401/401/200；390×844 视口完整渲染；named 隧道 4 条连接注册 |

## 阶段 7：内置终端（348 个测试全绿）

阶段 4 当时把内建终端留成了四个「明确报错」的命令占位，前端 `TerminalPanel`（xterm）
与 `capabilities/default.json` 一直是完整的——只差后端。新增 `toiler-desktop::terminal`：

- `portable-pty` 0.9（与 pi 同版本）→ `terminal_start/write/resize/close` 四个命令，
  `terminal-output` / `terminal-exit` 两个事件
- unix 用 `$SHELL`，Windows 用 `powershell.exe`；`TERM=xterm-256color`
- cwd 必须是**绝对路径**且真实存在；同时最多 16 个终端；单次键入上限 1MB
- 关面板 / 退出应用时杀**前台进程组**，不留 `npm run dev` 这类子进程
- 继承的 locale 不是 UTF-8 时就补 `LC_ALL`/`LC_CTYPE`（访达拉起的进程没有 `LANG`，
  中文输入法会回显成 `\M-ã<0080><0082>`）

它**只在桌面壳里**：起的是任意 shell，而 `toiler-server` 还要对局域网与隧道暴露 `/api`。
细节见 [README.md](./README.md) 阶段 4 的「内置终端」与 [PLAN.md](./PLAN.md) 阶段 7。

| 项 | 结果 |
|---|---|
| 测试 | 348 个全绿，0 warning（debug + clippy） |
| 真实 PTY | 起 `/bin/sh` 验证键入回显 / `pwd` / `stty size` 反映 resize（`30 100`） |
| 进程回收 | drop 会话后 shell 被 SIGKILL（`kill(pid, 0)` 返回 -1） |

## 后续阶段

见 [PLAN.md](./PLAN.md)。
