# pi-desktop `/api` 契约（从编译产物逐字提取）

来源：`/Applications/Pi Desktop.app/Contents/Resources/runtime/server/.next`
（pi-desktop `1.5.0-alpha.22`，Next.js App Router，58 个 `/api` route）

提取工具：`tools/pi-contract/extract.py`
原始抽取结果：`tools/pi-contract/raw/*.js`

> 这一层是**契约的事实来源**。PLAN 里对阶段的描述若与本文件冲突，以本文件为准
> （例如 PLAN 写「Basic Auth」，但实际实现里没有——见「安全」一节）。

## 安全：host + origin 校验（不是 Basic Auth）

每个需要防护的 handler 开头都调 `isTrustedApiRequest(request)`
（模块 `66544` 的 `EN`）。判定顺序：

1. **host 可信**：`Host` 头解析出的 hostname 必须是
   - `localhost` / `*.localhost`
   - IPv4 `127.*`
   - IPv6 `::1` / `0:0:0:0:0:0:0:1` / `::ffff:127.*`
   - 或者是任意 IP（`isIP` 非零）
   - 或者在允许列表 `PI_WEB_HOSTNAME` / `PI_WEB_ALLOWED_HOSTS`（逗号分隔）里
2. **例外**：`GET /api/sessions/<id>/export` 且
   `sec-fetch-mode: navigate` + `sec-fetch-dest: document` + `sec-fetch-user: ?1`
   （浏览器直接下载，不带 origin）
3. **origin 校验**（满足其一即通过）：
   - `Origin` 等于 `PI_WEB_DESKTOP_API_ORIGIN`
   - `sec-fetch-site: cross-site` → 拒绝
   - 没有 `Origin` → 通过（curl / 手机原生 App）
   - `Origin` 的 origin 等于请求自身的 origin（`scheme://Host`）→ 通过

失败返回 `403 {"error":"Untrusted API request"}`。

`isJsonRequest(request)`（`E2`）：`content-type` 为 `application/json`
或以 `+json` 结尾。

## GET /api/sessions

查询参数：`force=1` 跳过缓存（缓存 TTL 30s）。

```jsonc
{
  "sessions": [
    {
      "path": "/home/me/.pi/agent/sessions/--x--/2026-11-10T06-41-44-431Z_<id>.jsonl",
      "id": "<id>",
      "cwd": "/x",
      "name": "会话名或 null",
      "created": "2026-11-10T06:41:44.431Z",     // header.timestamp（ISO）
      "modified": "2026-11-10T07:35:34.471Z",    // 文件 mtime（ISO）
      "messageCount": 249,
      "firstMessage": "你能看到项目介绍吗",        // 第一条 user 消息的文本，截断 40 字符
      "parentSessionId": null,                    // 由 header.parentSession 反查
      "transient": false,
      "projectRoot": "/x",                        // git 项目根（无 git 时 = cwd）
      "projectKey": "<encode(projectRoot)>",
      "worktreeBranch": "feat/x"                  // 仅当 cwd 是 worktree 时出现
    }
  ],
  "sessionsUpdatedAt": …                            // 见 am()，客户端用于判定新鲜度
}
```

排序：`modified` 倒序（字符串比较）。

## GET /api/sessions/[id]

查询参数：`deferThinking`、`deferMedia`（出现即 true）、`limit`（默认 120，夹在 1..400）。

```jsonc
{
  "sessionId": "<id>",
  "filePath": "/…/<id>.jsonl",
  "info": { …同列表项… } | null,
  "leafId": "<leaf entry id>",
  "tree": [ /* 压缩后的树，见下 */ ],
  "context": { "messages": [...], "entryIds": [...], … },
  "paging": { "hasEarlier": true, "totalMessages": 249, "startIndex": 129, "pageSize": 120 },
  "totalActiveMs": 1234567
}
```

`tree` 是 SessionTree 的**压缩投影**：只有「根 / 分叉点 / 叶子」参与渲染，
单孩子链被折叠进 `compressedEntryIds`；每个节点 `{id, children:[…],
compressedEntryIds?:[…], branchPreview?:{role?, text}}`，`depth` 上限 200。

`totalActiveMs`：按条目 `timestamp` 累加「user / bashExecution 到下一条」的时长。

## POST /api/agent/[id]

请求体是 pi 的 agent command：`{"type":"prompt", …}`、`{"type":"get_state"}`、
`{"type":"abort"}` …（形状见 `rpi-agent` 的 command 层）。

响应：`200 {"success":true,"data":<command 返回值>}`
- 会话不存在：`404 {"error":"Session not found", "code":"prompt_rejected", "accepted":false}`（仅 prompt）
- 其它错误：`500 {"error": "<msg>"}`（prompt 额外带 `code`/`accepted`）

## GET /api/agent/[id]

`{"running":false}` 或 `{"running":true,"state":<get_state 的结果>}`

## GET /api/agent/[id]/events（SSE）

响应头：`Content-Type: text/event-stream`、`Cache-Control: no-cache, no-transform`、
`Connection: keep-alive`、`X-Accel-Buffering: no`。

帧格式：`data: <json>\n\n`；每 30s 一个注释心跳 `:\n\n`。

事件序列：
1. `{"type":"connected","sessionId":"<id>","isStreaming":<bool>}`
2. 然后是 pi 的 agent 事件，**经过裁剪**：
   - 丢弃 `turn_start`、`turn_end`
   - 丢弃 `message_start` / `message_end` 中 `message.role === "system"` 的
   - `message_update`：去掉 `partial` 字段；若 `assistantMessageEvent.type` 是
     `toolcall_start` / `toolcall_delta`，附上 `{id, toolName}`（来自
     `partial.content[contentIndex]`）
   - `tool_execution_update`：只留 `{type, toolCallId, toolName, partialResult}`
   - `agent_end`：只留 `{type}`
3. 启动失败：`{"type":"startup_error","errorMessage":"Failed to start agent: …"}` 后关闭

会话不存在：`404 Session not found`；请求已中断：`204`。

## GET /api/models

（见 `tools/pi-contract/raw/models.js`；阶段 2 只取前端首屏要用的字段子集，
在实现处标注。）

## GET /api/mobile/state

查询参数：`session`（id）、`cwd`、`limit`。

```jsonc
{
  "sessionId": "<id>" | null,
  "name": "会话名或 null",
  "cwd": "/x" | null,
  "status": "running" | "idle",
  "updatedAt": "ISO" | null,
  "queue": { "steering": 0, "followUp": 0 },
  "pendingUiRequest": null,
  "messages": [ /* 手机端消息裁剪结果 */ ],
  "earlierCount": 0,
  "thinkingLevel": "off",     // 可选
  "model": "GLM-5.3"          // 可选
}
```

选会话规则：显式 `session` > `cwd` 过滤后「正在运行」的会话 > 该 cwd 下
`modified` 最新的会话；都没有则返回空态（`sessionId: null`）。

## 实现进度（`crates/workman-server`）

| route | 状态 | 备注 |
|---|---|---|
| `GET /api/sessions` | ✅ | 字段全对齐（含 `sessionsUpdatedAt`、30s 缓存、`?force=1`） |
| `GET /api/sessions/{id}` | ✅ | `info`/`tree`/`context`/`paging`/`totalActiveMs` 全对齐 |
| `PATCH /api/sessions/{id}` | ✅ | 追加 `session_info` 条目（pi 的做法） |
| `DELETE /api/sessions/{id}` | ✅ | 关活 agent + 删文件 |
| `GET /api/sessions/{id}/state` | ✅ | 与 `GET /api/agent/{id}` 同实现 |
| `GET /api/sessions/search` | ✅ | 元数据 + 文本块 + compaction.summary，58 字符 snippet |
| `POST /api/agent/{id}` | ✅ | `prompt`/`steer`/`follow_up`/`abort`/`get_state` |
| `GET /api/agent/{id}` | ✅ | `{running}` / `{running, state}` |
| `GET /api/agent/{id}/events` | ✅ | SSE，裁剪规则与 pi 逐条对齐 |
| `POST /api/agent/new` | ✅ | 建 v3 会话 + 登记项目 + 执行 body 里的命令；`{success,sessionId,data,model,thinkingLevel}` |
| `GET /api/agent/running` | ✅ | `{runningSessionIds, pendingUiSessionIds}`（pi 的 `56429.am()`） |
| `GET /api/files/{*path}` | ⚠️ 部分 | 读文件 / 列目录 / `?type=watch` SSE 已通；`?type=diff`、git 忽略规则、图片缩略未做 |
| `GET /api/models` | ⚠️ 部分 | `{models, default, source}`；pi 的完整 catalog 元数据（价格/能力标记）未全量复刻 |
| `GET /api/mobile/state` | ⚠️ 部分 | 主体字段全对齐；手机端消息二次裁剪（`detailWindow`）未复刻 |
| `GET /api/home` | ✅ | `{home}` |
| `POST /api/cwd/validate` | ✅ | 支持 `~`/相对路径；成功后登记允许根 + 写项目库 → `{success,cwd,projectRoot,projectKey}` |
| `GET /api/cwd/browse` | ✅ | `{path,parentPath,directories[]}`（realpath 后列目录，符号链接指向目录也认） |
| `POST /api/default-cwd` | ✅ | mkdir + 登记允许根 → `{cwd}` |
| `GET /api/agent/{id}/bash-output` | ✅ | `?path=&download=1`；5 MiB 上限（413）、越权 403、`pi-bash-*.log` 白名单 |
| `GET /api/agent/running/events` | ✅ | SSE：首帧快照 + 变化即推 + 30s 心跳 |
| `GET /api/sessions/{id}/context` | ✅ | `{context,paging}`，形状同 detail 的 `context` |
| `GET /api/sessions/{id}/export` | ⚠️ | 单文件 HTML 下载（header/文件名规则同 pi，内容是线性转写稿，不是 pi 的树状视图） |
| `GET /api/sessions/{id}/entries/{entryId}/thinking` | ✅ | `?blockIndex=N` → `{thinking}` |
| `POST /api/sessions/{id}/auto-name` | ✅ | 用会话自己的模型起标题，写回 `session_info` → `{title,usage}` |
| `GET /api/auth/providers` | ⚠️ | `{providers}`；只列 `models.json` 里没写 `apiKey` 的 provider（见差异 6） |
| `GET /api/auth/all-providers` | ⚠️ | 恒为空：只支持 API Key，没有 OAuth 登录流程 |
| `GET /api/auth/api-key/{provider}` | ✅ | `{provider,displayName,configured,source,models,maskedKey}`，明文 key 永不出站 |
| `POST /api/auth/api-key/{provider}` | ✅ | `{apiKey}` → 写 `auth.json`（0600，原子替换） |
| `DELETE /api/auth/api-key/{provider}` | ✅ | 类型不符 → 409 |
| `POST /api/auth/logout/{provider}` | ✅ | 删 oauth 凭据；类型不符 → 409 |
| `POST /api/auth/login/{provider}` | ❌ | 501：rpi 没有 OAuth 登录流程 |
| `GET /api/models-config` | ✅ | 原样返回 `models.json` |
| `PUT /api/models-config` | ✅ | 保留 `$schema`、清洗无 id 的模型、`cost` 缺项补 0 |
| `DELETE /api/models-config` | ✅ | `{kind:"provider"\|"model", provider, modelId?}`，顺手清 `enabledModels` |
| `GET /api/models-config/enabled?provider=` | ✅ | `{provider,models[],enabledModels,unscoped,warnings,refreshing}` |
| `PUT /api/models-config/enabled` | ✅ | `{provider,modelIds}` → 折叠成 `provider/*` 或逐条写 `settings.json` |
| `POST /api/models-config/discover` | ✅ | 打上游 `/models` 并按 pi 的字段兼容表解析 → `{models,endpoint}` |
| `POST /api/models-config/test` | ✅ | 真跑一次补全 → `{ok,latencyMs,status,responseText}` |
| `GET /api/models-config/catalog` | ⚠️ | models.dev 目录已砍，返回空表 + `note` |
| `GET /api/git?cwd=` | ✅ | 分支/上游/增删/未跟踪/凭据类型/进行中的操作 |
| `POST /api/git` | ✅ | 20 个动作（含 `merge_branch`、`*_remote_branch`、`*_branch`），返回操作后的状态 |
| `GET /api/git/status?cwd=` | ✅ | `{isGitRepository,repositoryRoot,files[],additions,deletions}` |
| `GET /api/git/diff?cwd=&path=` | ✅ | `{supported,status?,patch?}`；未跟踪文件会合成补丁 |
| `POST /api/git/commit-message` | ✅ | NDJSON：`update` → `done`（失败 `error`）；用 `settings.json` 的默认模型 |
| `GET/POST/DELETE /api/worktrees` | ✅ | 列出 / 新建（`action:"checkout"` 走切分支）/ 删除（脏工作区 409 + `dirty:true`） |
| `GET/PUT/PATCH/DELETE /api/projects` | ✅ | 与 pi 共用 `<agentDir>/pi.sqlite`；星标、选中项、移除都对齐 |
| `GET/POST /api/project-trust` | ✅ | `{requiresTrust,trusted}`；`trust.json` 与 pi 同格式 |

### 与 pi 的已知差异（都要在阶段 5 回归时确认）

1. ~~**`projectRoot` 不做 git 探测**~~ —— 阶段 3 已修：`projectRoot` 走
   `git rev-parse`，worktree 时取主仓库根并带 `worktreeBranch`（60s 缓存）。
2. **`models` 元数据不全**：只给前端首屏需要的字段，没有价格/能力目录。
3. **`deferThinking` / `deferMedia`**：pi 用它们省掉大字段；这里原样返回（形状一致，体积更大）。
4. **tree 的 `compressedEntryIds`** 只在深度超过 200 时出现（与 pi 的代码路径一致，但 pi 的 `visible` 集合还有别的分支，未 100% 验证）。
5. **自定义 role 的消息**：写 v3 时降级为 `role:"custom"` + `customType`（pi 认的集合有限）。
6. **auth provider 列表只来自 `models.json`**：pi 的列表来自 models.dev 注册表（几百个），
   workman 砍掉了那个目录，所以 `/api/auth/providers` 只列「`models.json` 里声明过但没写
   `apiKey`」的 provider。全部 provider 都写了 key 时它是空数组——这不影响已有配置的使用。
7. **OAuth 登录**（`POST /api/auth/login/{provider}`）返回 501：rpi 只有 API Key 通道。
8. **`/api/auth/api-key/{provider}` 的 `displayName`** 取 `models.json` 的 `name`，缺省用 id
   （pi 取 models.dev 的展示名）。
9. **git 凭据**：pi 用 AES-256-GCM 加密存进 `git-credentials.json`（密钥在 macOS Keychain）。
   workman 把凭据交给系统 credential helper（`git credential approve`），因此
   `hasSavedCredential` / `savedCredentialUsername` 恒为 `false` / `null`；
   「记住凭据」对后续 `git` 命令仍然生效。
10. **`/api/git/commit-message` 的流**：pi 每收到一段就发一行 `update`；workman 攒完再一次
    `update` + `done`（字段相同，少了中间增量）。
11. **`/api/sessions/{id}/export` 的 HTML**：响应头与文件名规则与 pi 一致，但内容是自己渲染的
    线性转写稿（pi 用 `pi` CLI 生成带树状导航的 HTML）。`?inline=1` / `?desktop=1` / `returnTo`
    都支持。
12. **`/api/agent/{id}/bash-output` 的会话归属校验**：pi 只认
    `role:"bashExecution"` 的顶层 `fullOutputPath`；workman 额外接受新工具形态
    （`role:"toolResult"` 的 `details.fullOutputPath`），因为当前 pi 就是这么写的。
13. **`/api/models-config/catalog`** 恒为空表（models.dev 目录已砍，见 PLAN「明确不做的」）。
14. **`/api/agent/running/events` 的 `pendingUiSessionIds`** 恒为空：workman 没有
    「等待用户输入的 UI 请求」这条链路。

### agent 目录与共享文件

| 文件 | 用途 | 与 pi 的关系 |
|---|---|---|
| `models.json` | provider / 模型定义 | 共用（`$WORKMAN_AGENT_DIR` 可改） |
| `auth.json` | API Key / OAuth 凭据（0600） | 共用 |
| `settings.json` | `enabledModels`、`defaultProvider/Model` | 共用（只读写这几个键） |
| `trust.json` | 项目信任决定 | 共用 |
| `pi.sqlite` | 项目列表 + 选中项（`added_projects` / `app_preferences`） | 共用 |
| `sessions/` | v3 会话文件 | 默认 `~/.pi/agent/sessions` |

解析顺序（`crates/workman-server/src/paths.rs`）：`$WORKMAN_AGENT_DIR` >
`~/.workman/agent`（有配置文件时） > `~/.pi/agent`（有配置文件时） > `~/.workman/agent`。

### rpi 侧发现的已知问题（不属本仓库职责）

- **`openai-completions` 流式 toolCall 的 `id` 会重复拼接**（qoder 端点每段 delta 都发完整 id，
  rpi 做的是 `concat` 而不是 `set`）。实测 4 段 → 280 字符。
  影响：provider 通信正常、v3 文件可读，但 id 是脏的。如需修复要改 rpi 依赖或在服务层后处理。
