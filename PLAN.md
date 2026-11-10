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
  - `projects` 四个方法（与 pi 共用 `pi.sqlite`）、`project-trust`（`trust.json`）
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
| `pi.sqlite` 共用 | 读出 15 个真实项目（含星标、选中项），增删改都写回同一个库 |
| git 面板 | 真实仓库：分支/上游/change 列表/增删行数/未跟踪；`diff` 命中真实补丁 |
| `models-config/test` | qoder/DeepSeek-Flash 真跑通：`{ok:true,latencyMs:732,responseText:"OK"}` |
| `models-config/discover` | 上游返回 12 个模型，被正确解析排序 |
| `auto-name` | 2.0s 生成「阶段3：实现后端路由与服务接入」并写回 `session_info` |
| `git/commit-message` | 真实暂存区 → `feat(main): 在启动时打印问候语`（NDJSON） |
| `bash-output` | 真实 `pi-bash-*.log`：读 787KB / 下载 200；越权会话 403；非白名单 400 |
| `agent/new` + `prompt` | 新会话建好、项目登记、一轮对话落盘（5 条 v3 记录） |

## 阶段 4 — Tauri 单进程（1 周）

- [ ] axum server 跑在 Tauri 的 tokio runtime 上，不 spawn 子进程
- [ ] 前端产物 `include_dir!` 嵌入二进制
- [ ] 删除 `desktop-dist/runtime/node` 与 `spawn_server`
- [ ] 保留 `lan_proxy.rs` 与 `tunnel.rs`（手机访问要用）

验收：单进程启动，安装包显著瘦身，冷启动 < 1s。

## 阶段 5 — 回归（1 周）

- [ ] pi-desktop 的 192 个前端测试
- [ ] 手机扫码 + LAN 代理 + Cloudflare 隧道
- [ ] 327 个历史会话全部可见可续聊

## 明确不做的

| 项 | 原因 |
|---|---|
| MCP 服务器管理 | rpi 无对应能力，自己做成本高于收益 |
| TS 扩展 / 技能市场 | rpi 是 cdylib ABI，生态不兼容 |
| models.dev 定价目录 | 可选，后补 |
| OAuth 登录 | 当前配置不需要 |
