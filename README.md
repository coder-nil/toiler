# workman

基于 [pi-rust (`rpi`)](https://crates.io/crates/rpi-harness) 的桌面编程工作台。
`rpi` 作为**未修改的 crates.io 依赖**引入，升级只需改版本号。

当前锁定 **rpi 0.21.2**（2026-11-09 发布）。从 0.6.2 升级到 0.21.2 零代码改动，
四项验证在 0.21.2 下重新跑通。

当前状态：**阶段 0（PoC）+ 阶段 1（v3 会话层）+ 阶段 2/3（HTTP 服务与全部 route）
+ 阶段 4（Tauri 单进程）完成**，只剩阶段 5 回归。
四项关键验证全部通过；559 个真实 pi 会话全量可读，且能原地追加。

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

```bash
cargo run -p workman-poc --bin workman-poc -- info              # 只做配置映射，不发请求
cargo run -p workman-poc --bin workman-poc -- stream gpt-5.4    # 真实流式请求
cargo run -p workman-poc --bin v3probe                          # 扫描 v3 会话兼容性
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
| OAuth 登录 | ✅ | 仅扩展内 | 不用（当前两个 provider 都是 API Key） |
| 内置工具 | read/write/edit/bash/grep/find/ls… | 同左 + powershell | ✅ 够用 |

## 阶段 1：v3 会话层（`crates/workman-session`）

目标：不迁移、不改格式，**直接读写 pi 的 v3 `.jsonl`**，让 pi 与 workman 并行打开同一份历史。

```bash
cargo run -p workman-session --bin workman-session -- init      # 初始化 ~/.workman/agent/sessions
cargo run -p workman-session --bin workman-session -- list       # 列会话
cargo run -p workman-session --bin workman-session -- load <file>
cargo run -p workman-session --bin workman-session -- append <file> "文本"   # 追加一轮
cargo run -p workman-session --bin workman-session -- verify ~/.pi/agent/sessions  # 全量回归
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
| 复制真实会话到 `~/.workman` | 可列出 / 可加载 / 可追加 |
| 追加后老字节 | 完全一致（append-only） |
| pi 侧复读追加后的文件 | `loadEntriesFromFile` 270 条全解析；`buildSessionContext` 纳入追加的两条 |

## 阶段 2：HTTP/SSE 服务（`crates/workman-server`）

把 pi-desktop 的 `/api` 契约用 axum 复刻。**契约从 `.next` 编译产物逐字逆向**，
结论固化在 [`docs/contracts/README.md`](./docs/contracts/README.md)，逆向工具在
`tools/pi-contract/extract.py`（可重新抽任意 route / 共享模块）。

```bash
cargo run -p workman-server -- --port 8787          # 会话根默认 ~/.pi/agent/sessions
cargo run -p workman-server --bin catalog-probe     # 只看 models.json → provider 映射
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
`WORKMAN_MODEL=provider/id` 覆盖。

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

- **配置与 pi 共用**：`models.json` / `auth.json` / `settings.json` / `trust.json` /
  `pi.sqlite` 都落在同一个 agent 目录（解析顺序见 `paths.rs`），所以已有的
  API Key、项目列表、模型可见范围在 workman 里立刻可用。
- **`pi.sqlite` 直连**：项目列表、星标、选中项用 `rusqlite` 读写 pi 自己的库
  （`added_projects` / `app_preferences`），两边不会各存一份。
- **git 走 CLI**：20 个动作、worktree、diff 全部包在 `gitsvc.rs`，每个仓库一把锁；
  凭据走临时 `GIT_ASKPASS`（不落明文到命令行）。
- **真模型的三处调用**：`auto-name`（起标题）、`git/commit-message`（写提交信息）、
  `models-config/test`（连通性自检）都复用 `oneshot.rs` 的一次性补全。
- **`projectRoot` 修好了**：改成真 `git rev-parse` 探测，worktree 会话会带
  `worktreeBranch`（阶段 2 的已知差异 1 已消）。

```bash
cargo test                      # 101 个测试
cargo run -p workman-server -- --port 8787
curl -s "localhost:8787/api/git?cwd=$PWD" | jq .
curl -s "localhost:8787/api/auth/providers" | jq .
```

### 仍未做

`files?type=diff` 与图片缩略、手机端消息二次裁剪、模型价格目录（models.dev 已按
计划砍掉）、OAuth 登录（返回 501）。前端联调已在阶段 4 打通（见下）。

## 阶段 4：Tauri 单进程（`crates/workman-desktop`）

一个可执行文件里同时跑壳、后端与前端：**没有 node，没有子进程**。

```bash
# 不嵌真实前端（二进制里是占位页，API 照常可用）
cargo run -p workman-desktop

# 嵌 pi 的静态导出产物后构建 release
tools/frontend/build-release.sh /path/to/pi-desktop/src-tauri/frontend
./target/release/workman-desktop
```

### 结构

| 模块 | 作用 |
|---|---|
| `workman_server::boot` | 启动引导：会话根 → 模型目录 → `AppState` → `bind` / `serve`；CLI 与桌面壳共用 |
| `workman_server::assets` | `include_dir!` 嵌前端；`_next/static` immutable、目录 index、RSC `.txt`、客户端路由兜底 |
| `workman-desktop::server` | 把 `bind(127.0.0.1:0)` → `tauri::async_runtime::spawn(serve)`，顺手预热会话列表 |
| `workman-desktop::{lan_proxy,tunnel}` | 从 pi 原样移植的手机访问件（局域网反代 + Cloudflare 隧道） |

三处与 pi 的关键差别：

1. **同源**：窗口直接加载 `http://127.0.0.1:<port>/`（pi 是 `index.html` + 另一个
   API origin）。`/api` 与前端一个端口，SSE/fetch 不用处理跨域。
2. **端口由内核分配**（`port = 0`）：不再抢 8787，也不会出现「端口被占用」。
3. **没有启动探测**：`bind` 完成即可用（pi 是轮询 45s 等 node 起来）。

### 验收

| 项 | 结果 |
|---|---|
| 冷启动（exec → 首页 200） | 中位 **114ms** |
| 二进制 | 51MB（含 24MB 前端）；pi 的 `.app` 是 255MB |
| 前端渲染 | 真实 pi 产物内嵌后界面完整：会话列表 / 项目切换 / 模型选择 / 文件面板（含 git 状态） |
| `/api/sessions` | 559 个会话；首次扫描 7.8s → **1.68s**（并行），预热后 13ms |
| 手机访问 | LAN 代理 200（Basic Auth）/ 无凭据 401；隧道起停正常，退出不留孤儿 |
| 测试 | 151 个全绿 |

## 后续阶段

见 [PLAN.md](./PLAN.md)。
