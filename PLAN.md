# workman 实施计划

前提：`rpi` 作为 crates.io 依赖，不改源码；升级靠改版本号。
`pi-desktop` 保持原样不动，两者并行。

## 阶段 0 — PoC ✅ 已完成

- [x] crates.io 引入 `rpi-ai` / `rpi-harness`，编译通过
- [x] `openai-responses` 对 `coding.apisets.com` 真实流式出字
- [x] `models.json` 直接映射为 `rpi::Model`
- [x] v3 会话可读性扫描（58,081/58,086）

## 阶段 1 — v3 SessionStorage ✅ 已完成

交付物：`crates/workman-session`（约 2,600 行：实现 + CLI + 20 个测试）

- [x] 实现 `SessionStorage`（`rpi_harness::session::types`）
  - 读：解析 v3 header + 条目，`seq` 按成功落地顺序补（v3 无此字段）
  - 写：append v3 格式条目（保持 pi 兼容，**老字节一字不改**）
  - `stopReason: toolUse` 双向归一
  - `bashExecution` / `system` → `CustomMessage` 映射
  - 分支（`parentId` 指向非叶子）→ 自动回退 lane 叶子再 append
  - pi 私有条目（`session_info` / `context_edit`）→ `Custom` 条目，名字回填 fact
- [x] `~/.workman/agent/` 目录结构与首次启动初始化（`init` 子命令）
- [x] 会话列表（复用 pi 的 `--<cwd>-` 目录命名与 `<ISO>_<id>.jsonl` 文件名）
- [x] 单元测试：真实形状 fixture（`basic.jsonl` / `branch.jsonl`）+ 真实 pi 会话回归

**验收结果（真实数据）**：

| 项 | 结果 |
|---|---|
| 554 个真实 pi 会话全量回放 | 554/554 完整加载，102,572 条目，0 行跳过 |
| 复制一个真实会话到 `~/.workman` | 能列出、能加载、能追加一轮对话 |
| 追加后老字节 | 完全一致（append-only） |
| pi 侧复读追加后的文件 | `loadEntriesFromFile` 270 条全解析；`buildSessionContext` 把追加的两条纳入 context |

## 阶段 2 — HTTP/SSE 服务（进行中，主体已完成）

保留手机遥控 → HTTP 服务不能省。用 axum 复刻 pi-desktop 的 `/api` 契约
（契约事实来源：`docs/contracts/README.md`，从 `.next` 编译产物逐字逆向）。

- [x] `crates/workman-server`：axum 0.8 + SSE
- [x] 会话：`GET /api/sessions`、`GET /api/sessions/[id]`（含 tree/paging/totalActiveMs）
- [x] 会话：`PATCH`（改名）、`DELETE`、`GET /api/sessions/[id]/state`、`GET /api/sessions/search`
- [x] agent：`POST /api/agent/[id]`、`GET /api/agent/[id]/events`（SSE，裁剪规则逐条对齐）
- [x] agent：`POST /api/agent/new`、`GET /api/agent/running`
- [x] 文件：`GET /api/files/[...path]`（读文件 / 列目录 / `?type=watch` SSE）
- [x] 模型：`GET /api/models`（`models.json` → rpi provider 的真映射）
- [x] 手机：`GET /api/mobile/state`
- [x] 安全：host 白名单 + Origin 同源校验（**不是** Basic Auth —— pi 实际实现如此）
- [x] 真实 agent 一轮对话跑通（qoder/GLM-5.3，含工具调用，v3 文件写穿，pi 可复读）
- [ ] 剩余 route（`bash-output`、`running/events`、`sessions/[id]/context|export`、
      `entries/*/thinking`、`auto-name`）→ 阶段 3
- [ ] 前端联调：pi-desktop 的 `.next` 是 SSR 产物，需要「前端 dev server + 反向代理到
      本服务」或阶段 4 的 Tauri 单进程方案才能真正点开界面 → 阶段 4/5

**已验收**（`curl` + 真实数据）：555 个历史会话可列、可读；`/api/sessions/[id]` 的
tree/paging/totalActiveMs 形状与 pi 一致；新会话里 `prompt` 触发的
`agent_start → message_start → message_update×N → tool_execution_* → agent_end`
事件序列与文件写入均正确。

## 阶段 3 — 剩余 route ✅ 已完成

交付物：`crates/workman-server` 新增 8 个模块 + 6 个 route 文件（约 6,600 行）

- [x] auth（API Key 存取，不返回明文）
  - `providers` / `all-providers` / `api-key`(GET·POST·DELETE) / `logout` / `login`(501)
  - `auth.json` 0600 + 原子替换；`$ENV` 插值与 `!command` 凭据都能解析
- [x] models-config（读写 `models.json`，保留 `$schema`）
  - GET / PUT（清洗无 id 的模型 + cost 缺项补 0）/ DELETE（provider 或 model）
  - `enabled`（GET/PUT，`provider/*` 折叠与 pi 的 `13888.uz` 同语义）
  - `discover`（上游 `/models` + pi 的字段兼容表）/ `test`（真跑一次补全）
  - `catalog` 已按计划砍掉（返回空表）
- [x] git / worktrees
  - `GET /api/git`、`POST /api/git`（20 个动作）、`git/status`、`git/diff`、`git/commit-message`（NDJSON）
  - `GET/POST/DELETE /api/worktrees`（新建目录在 `<projectRoot>-worktrees/<branch>`）
  - 每个仓库一把锁串行写操作；`GIT_TERMINAL_PROMPT=0` + 临时 askpass 传凭据
- [x] projects / cwd / default-cwd / home
  - `projects` 四个方法、`project-trust`（`trust.json`）
  - `cwd/browse`、`cwd/validate`（POST，登记项目 + 允许根）、`default-cwd`、`home`
- [x] 阶段 2 的欠账一并补完
  - `agent/{id}/bash-output`、`agent/running/events`
  - `sessions/{id}/context`、`sessions/{id}/export`、`entries/{id}/thinking`、`auto-name`
- [x] `projectRoot` / `worktreeBranch` 改为真 git 探测（修掉阶段 2 的差异 1）
- [x] 明确砍掉：mcp、plugins、skills 市场、models.dev 目录

**验收结果（真实数据 + 真实模型）**：

| 项 | 结果 |
|---|---|
| 单元/集成测试 | 101 个全绿（新增 29 个），`cargo build` 0 warning |
| 558 个真实 pi 会话 | 列表 / detail / context / export / thinking 全部可读 |
| 项目库 | 读出 15 个真实项目（含星标、选中项），增删改都写回；阶段 6 起改为独立的 `work.sqlite` |
| git 面板 | 真实仓库：分支/上游/change 列表/增删行数/未跟踪；`diff` 命中真实补丁 |
| `models-config/test` | qoder/DeepSeek-Flash 真跑通：`{ok:true,latencyMs:732,responseText:"OK"}` |
| `models-config/discover` | 上游返回 12 个模型，被正确解析排序 |
| `auto-name` | 2.0s 生成「阶段3：实现后端路由与服务接入」并写回 `session_info` |
| `git/commit-message` | 真实暂存区 → `feat(main): 在启动时打印问候语`（NDJSON） |
| `bash-output` | 真实 `pi-bash-*.log`：读 787KB / 下载 200；越权会话 403；非白名单 400 |
| `agent/new` + `prompt` | 新会话建好、项目登记、一轮对话落盘（5 条 v3 记录） |

## 阶段 4 — Tauri 单进程 ✅ 已完成

交付物：`crates/workman-desktop`（Tauri 壳 + 同进程 axum）+ `workman-server` 的
`boot.rs` / `assets.rs`（约 2,900 行：壳 700 行 + 移植的 lan_proxy/tunnel 2,100 行 +
静态托管 300 行）

- [x] axum server 跑在 Tauri 的 tokio runtime 上，不 spawn 子进程
  - `boot::bind`（`port = 0`，内核分配）→ `tauri::async_runtime::spawn(boot::serve)`
  - 启动引导抽到 `workman_server::boot`，CLI（`main.rs`）与桌面壳共用同一套
  - 会话列表在服务起来后立刻后台预热（`spawn_blocking`），首屏不等扫描
- [x] 前端产物 `include_dir!` 嵌入二进制
  - `crates/workman-server/src/assets.rs`：`/_next/static/*`（immutable 缓存）、
    目录 index、RSC `.txt` 载荷、客户端路由兜底 `index.html`、404 页
  - 产物目录由 `WORKMAN_FRONTEND_DIR` 决定（`build.rs` 注入 `rustc-env`），
    默认是 crate 内占位页 —— 没有前端也能 `cargo build`，API 照常可用
  - 静态资源挂在 `Router::fallback` 上：`/api/*` 仍是具体路由，不会被兜底吞掉
- [x] 删除 `desktop-dist/runtime/node` 与 `spawn_server`
  - 新壳里没有 `Command::new(node)`、没有端口轮询、没有 `STARTUP_TIMEOUT = 45s`
  - `tauri.conf.json` 不再打包 `desktop-dist` 资源
- [x] 保留 `lan_proxy.rs` 与 `tunnel.rs`（手机访问要用）
  - 原样移植，只把 `agent_dir()` 接到 `workman_server::paths::agent_dir()`
  - 与 pi 共用 `desktop-access.json` / `.runtime.json` / `.tunnel.json`
  - 退出（含 SIGTERM/SIGINT）会先断隧道再关代理，不留孤儿 `cloudflared`

**验收结果（真实数据 + 真实产物）**：

| 项 | 结果 |
|---|---|
| 单进程 | 只有一个 `workman-desktop` 进程；无 `node`，除用户启用的 `cloudflared` 外无子进程 |
| 冷启动（exec → `GET /` 200） | 中位 **114ms**（5 次：496/106/114/110/120，首次含冷文件缓存） |
| 二进制 | 51MB（含 24MB 前端产物）；pi 的 `Pi Desktop.app` 是 255MB（`runtime/node` 106MB + `server` 109MB） |
| 前端渲染 | 真实 pi 静态导出产物内嵌后，界面完整：会话列表（559）/ 项目切换 / 模型选择器 / 文件面板（含 git 状态标记） |
| 顺带修好的契约 | `/api/files` 新增空路径与「去前导斜杠的绝对路径」两种形状；`isDir` 字段对齐 pi（否则文件面板显示 “Not found”） |
| 静态资源 | `/_next/static/chunks/webpack-*.js` 200，`immutable` 缓存；`/` 200 `no-cache` |
| `/api` 同源 | `/api/sessions` 559 个会话；`/api/does-not-exist` → 404 JSON（不被兜底成 HTML） |
| 会话列表性能 | 并行扫描：首次 7.8s → **1.68s**；预热后 13ms |
| 手机访问 | LAN 代理随机端口 200（Basic Auth）+ 无凭据 401；隧道 `named` 模式起停正常 |
| 测试 | 151 个全绿（新增 34 个桌面壳 + 9 个静态托管 + 2 个 boot），`cargo build` 0 warning |

**已知差异（阶段 5 已处理，见下节）**：内置终端、`files?type=diff`、手机端开关写入接口。

## 阶段 5 — 回归 ✅ 已完成

交付物：`crates/workman-server` 新增 `filemeta.rs` / `docx.rs` / `mobile_access.rs` /
`mobile_view.rs` / `routes/mobile.rs` + `tests/regression.rs`（约 4,500 行），
`/api/files` 全量重写，`session_view` 补 toolCall 归一化。

### 1. 前端测试与契约固化

- [x] pi-desktop 前端测试跑通并固化基线：**1326 用例 / 1318 pass / 7 fail / 1 skip**
  - 7 个失败全部在 pi 自身（plugins catalog 需要联网、i18n 覆盖率、`sync-version.mjs`），
    与 workman 无关；`PI_WEB_SHELL_PID` 会污染 `desktop-access` 用例，清掉后 10/10
- [x] 新增 `tests/regression.rs`：**41 个契约用例**（把前端真实依赖的行为逐条落成断言）
  - `/api/files` 六种 `type` + 写侧三种方法 + SSE 命名事件
  - `/api/sessions` 全字段 / tree / context / paging / 改名 append-only / 搜索 / 删除
  - `/api/mobile/access` + `/api/mobile/pair` 全契约
  - 默认模型优先级、`with_binding` 在 clone 后可调用

### 2. `/api/files` 全量对齐（阶段 3 的欠账）

| 能力 | 阶段 4 状态 | 阶段 5 |
|---|---|---|
| `type=read` 文本 | JSON，但**没有 `language`** | `{content, language, size}`，`language` 与 pi 的映射表逐项一致 |
| `type=read` 图片/音频/文档 | 一律 base64 塞 JSON | **流原字节** + `Range`（206/416）、`Content-Disposition` |
| `type=download` | 无 | `attachment` + RFC 5987 文件名（中文/emoji 都正确） |
| `type=meta` | 无 | `{size, language, mime, previewKind}` |
| `type=preview` | 无 | DOCX → HTML（自写 OOXML 解析：标题/粗斜体/列表/表格，无 Node 依赖） |
| `type=watch` | 默认 SSE 事件名 | **命名事件** `connected` / `change`（前端 `addEventListener` 才收得到） |
| `type=list` | 有，但 `size/mtime` 是 stat 结果 | 与 pi 一致：`size:0` / `modified:""`，补 `gitRepository`、`hideDotDirs` |
| 写：`PUT` | 无 | `{content, allowEmpty}`，空覆盖 409 `empty_overwrite_blocked` |
| 写：`PATCH` | 无 | 重命名，撞名 409 `already_exists` |
| 写：`POST` | 无 | `upload-check` / multipart `upload`（25MB 单文件 / 100MB 总量，三种冲突策略） |
| 授权 | 只有目录枚举 | 两层（词法 + realpath）+ `sessionId` 引用通道 |

### 3. 手机访问接口

- [x] `GET/PUT /api/mobile/access`（三份文件的唯一写入口，0600、原子替换、字段级合并）
- [x] `GET /api/mobile/pair`（局域网地址 / 公网地址 / 五种 reason，**不含任何凭据**）
- [x] `mobile_view`：手机消息投影（pi 的 `buildMobileWindow`）

### 走查抓到并修掉的 6 个真实缺陷

真机走查（浏览器 + 桌面壳 + 真实模型）比测试更狠，下面这些**测试全绿时都还存在**：

| # | 缺陷 | 症状 | 根因 |
|---|---|---|---|
| 1 | `context` 不做 toolCall 归一化 | **打开任何带工具调用的历史会话整页白屏** | v3 写 `{id,name,arguments}`，前端读 `{toolCallId,toolName,input}`；`toolName` 为 undefined 时 `isFileWritingToolName` 直接 TypeError |
| 2 | `with_binding` 在 clone 后 panic | **应用一启动就 Abort trap** | 桌面壳先 `clone()` 预热会话列表，再用 `Arc::get_mut().expect()` |
| 3 | 壳没注入 `PI_WEB_*` 环境变量 | 配对永远停在 `starting`、`desktopShell` 恒为 false | 同进程不像 pi 那样有 `Command::env()` 这一步 |
| 4 | `copy_stream` 把 `EAGAIN` 当致命错误 | **手机端 JS chunk 只收到 290KB/1.29MB，白屏** | `SO_RCVTIMEO` 让 socket 非阻塞，发送缓冲满时 `write` 返回 `WouldBlock` |
| 5 | `LiveAgent` 忽略 `settings.json` 默认模型 | 续聊用错模型 | 只用了 `catalog.default_model()`，没用 `default_model_ref()` |
| 6 | `/api/mobile/state` 直接下发原始消息 | 一屏 474KB（图片 base64 占 682KB） | 缺手机投影，改为 54KB（-89%） |

### 验收结果（真实数据 + 真实前端产物 + 真实模型）

| 项 | 结果 |
|---|---|
| 测试 | **257 个全绿**（`cargo test --workspace`），0 warning |
| 历史会话全量 | 561 个文件、560 个有 v3 header（1 个无 header 是 pi 自身也跳过的）、**560/560 可读**；104,986 条目 / 103,318 消息 **0 失败** |
| 真实续聊 | 382 行老字节**一字不改**；追加 user+assistant（`stopReason=stop`）；**pi 自己的 `SessionManager` 复读该文件得 388 条目 / 385 context 消息**，最后一条正是 workman 写入的「收到」 |
| 桌面界面 | 会话列表 / 378 条消息会话（思考、工具调用、token 统计、分页「隐藏 351 条」）/ 文件面板 / Rust 语法高亮 / PNG 渲染 / Markdown 表格预览 —— 全部实测 |
| 手机 Basic Auth | 无凭据 401（`WWW-Authenticate: Basic realm="Pi Desktop (username: pi)"`）、错密码 401、对密码 200 |
| 手机遥控页 | 390×844 视口下完整渲染；`/api/mobile/state` 474KB → **54KB** |
| 配对信息 | `{url: http://10.17.44.112:60612/m?…, reason: ok, publicUrl: https://m.π.works/m?…}`，**不含密码** |
| Cloudflare 隧道 | named 模式真实建立，4 条连接全部 Registered（lax07/lax13/lax01/lax08，QUIC），退出无孤儿 |
| 前端测试 | 1326 用例 / 1318 pass（7 个失败全在 pi 自身） |

## 阶段 6 — 目录独立 ✅ 已完成

workman 不再把状态写进 pi 的 `~/.pi/agent`。

```text
~/.agents/                  ← 通用根：skills / mcp / commands / tools
└── workman/               ← workman 专属
    ├── configs/            auth / models / settings / trust / desktop-access
    ├── sessions/           v3 会话
    ├── plugins/            （预留）
    └── work.sqlite         项目列表 + 选中项
```

- [x] `paths.rs` 重写：`agents_home()` / `workman_home()` / `configs_dir()` /
      `sessions_dir()` / `plugins_dir()` / `project_db()` / `legacy_pi_agent_dir()`
- [x] 首次启动播种（`seed_from_pi_if_needed`）：从 `~/.pi/agent` **复制**配置与实际
      会话，0600 落盘，**旧文件保留**；播过就不再动
- [x] 项目库 `work.sqlite` 首次创建时从旧 `pi.sqlite` **只读**导入
- [x] 顺带修掉 `catalog.rs` 自己拼旧路径的隐患（会话根换新了、模型目录还在读 pi）

**验收**：`auth/models/settings/trust/desktop-access` 5 份配置副本与源 sha256 一致、
权限 0600；562 个会话全数复制；服务启动日志与 `/api/models` 的 `source` 都指向
`~/.agents/workman/configs/models.json`；信任读写只落新目录，`~/.pi/agent` 一字未动。

## 明确不做的

| 项 | 原因 |
|---|---|
| MCP 服务器管理 | rpi 无对应能力，自己做成本高于收益 |
| TS 扩展 / 技能市场 | rpi 是 cdylib ABI，生态不兼容 |
| models.dev 定价目录 | 可选，后补 |
| OAuth 登录 | 当前配置不需要 |
