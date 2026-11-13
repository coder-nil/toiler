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

请求体是 pi 的 agent command：`{"type":"prompt", …}`、`{"type":"set_model","provider":…,"modelId":…}`、
`{"type":"get_state"}`、`{"type":"clear_queue"}`、`{"type":"abort"}` …（形状见 `rpi-agent` 的 command 层）。

`prompt` 的 `streamingBehavior`（`"steer"` | `"followUp"`，缺省 `steer`）在会话运行中
决定进哪条队列：`steer` 在工具批处理之后注入，`followUp` 等本轮结束再投递。

`clear_queue` 清空两条队列并返回**被清掉的文本**（`{steering:[…], followUp:[…]}`），
前端的「撤回排队」按钮拿它回填输入框；空队列返回两个空数组（不是 `null`）。

`set_model` 与 pi 一致：按 `provider/modelId` 查模型目录，追加一条 `model_change`
到 v3 会话文件（前端切完的 `loadSession` 靠它回显模型），返回 `{id, provider}`；
查不到时 `500 {"error":"Model not found: <provider>/<modelId>"}`。

`set_ui_locale`（`{"type":"set_ui_locale","locale":"zh-CN"|"en"}`）返回
`{systemPrompt}`。前端打开会话、切换界面语言都会发它，返回值直接刷新「系统提示词」
面板；它同时把提示词换成对应语言（preamble / 工具说明 / 行为准则 / 语言指令）。

`get_state` 的响应里含 `systemPrompt`、`extensionStatuses`、`extensionWidgets`、
`pendingUiRequests`，以及 `queuedMessages`。`queuedMessages` 是**文本数组**：

```jsonc
"queuedMessages": { "steering": ["第一条"], "followUp": [] }   // 空时为 []
```

前端排队条（`ChatInput`）按条渲染并数 `.length`，撤回按钮把文本回填输入框 ——
发数量（`0`）会让排队条永远不出现、撤回拿不到内容。手机端 `/api/mobile/state`
的 `queue` 则是**数量**（只显示「N queued」），两者形状不同是故意的。

`systemPrompt`、`extensionStatuses`、`extensionWidgets`、
`pendingUiRequests` 的含义：`systemPrompt` 的值是**实际发给模型的那一份**提示词：
`preamble → <tools> → <rules> → <project_context> → <cwd>` 分节
（`system_prompt.rs`），与 pi 的 `effectiveSystemPrompt()` 同义。缺了它前端面板
会一直显示「系统提示词尚未加载」，所以它是契约的一部分，不是可选字段。

`get_tools` 返回 `[{name, description, active}]`，`set_tools`
（`{"type":"set_tools","toolNames":[…]}`）返回 `null`。前端打开会话就用
`get_tools` 反推工具预设（`getPresetFromTools`），参数面板切换预设则发 `set_tools`。
注册的工具共 10 个：7 个编码工具（read/bash/edit/write/grep/find/ls）+
3 个桌面工具（`system_time` / `task_progress` / `ask_user`，实现见
`toiler-server/src/desktop_tools/`）。桌面工具**不受预设控制**（pi 的
`withExtensionTools` 也是这么做的），所以「只读」「关闭」预设下它们仍是 active。
`mcp` / `browser` / `goal` 系工具不提供（见 AGENTS.md「明确不做的事」）。
`set_tools` 同时重算系统提示词的 `<tools>` 段与 `<project_context>` 形态
（没有 `read` 时退回全文）。

`extension_ui_response`（`{"type":"extension_ui_response","id":…,"value"|"confirmed"|"cancelled":true}`）
回答 `ask_user` 的提问，返回 `null`；请求已作废（超时/中止）时同样返回 `null`
（前端只是晚了一步，不算错误）。`extension_ui_input`（custom UI 的逐键输入）
收下即忽略：toiler 不提供 custom 渲染。

`navigate_tree`（`{"type":"navigate_tree","targetId":"<entryId>"}`）把 main lane 的叶子
**移到**目标条目上（不是它的父节点），返回 `{cancelled, leafId}`。前端三个入口都发它：
消息上的「从此处编辑」、消息树点节点、分支下拉切换叶子。拿到 `leafId` 后前端一定跟一次
`GET /api/sessions/{id}/context?leafId=…` 重载分支，所以返回值必须是**导航后**的叶子。

`hideTarget: true`（仅「从此处编辑」带）：叶子退到目标条目的**父节点**，目标条目与它之后
的内容一起从分支上消失（用户改完重发，新消息挂在父节点下、旧消息留在旧分支）。
目标条目是第一条消息时父节点为 `null`，此时返回 `leafId: null`，前端据这个值
**不请求 context** 而是直接清空对话区（不带 `leafId` 请求会退回文件最后一条，
那正是「编辑了但列表没变」的老毛病）。

v3 文件里没有 leaf 记录（pi 也没有）：只挪内存指针，重开会话时叶子退回最后一条条目；
真在新分支上续聊时新条目带 `parentId` 落盘，那一行自己就成了叶子。
目标条目不存在时 `500`（pi 抛 `Entry <id> not found`）。

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
3. 扩展 UI 请求：`{"type":"extension_ui_request","id":…,"method":"select"|"confirm"|"input"|"editor",…}`，
   请求作废（回答/超时/中止）时补一帧 `{"type":"extension_ui_resolved","id":…}`；
   `method` 还有 `setStatus` / `setWidget`（`task_progress` 的展示，非阻塞）。
3.5 排队变化：`{"type":"queue_update","steering":["<文本>"],"followUp":[]}`。
   入队（`prompt` 运行中 / `steer` / `follow_up`）、清空（`clear_queue` / `abort`）
   与一轮结束时各推一帧；桌面端据此刷新排队条，手机端收到后重拉一次快照。
4. 启动失败：`{"type":"startup_error","errorMessage":"Failed to start agent: …"}` 后关闭

会话不存在：`404 Session not found`；请求已中断：`204`。

## GET /api/skills

来源：pi 的 `app/api/skills/route.ts` + `lib/skills-service.ts`（pi-desktop 源码）。
**此前这份契约漏了这一组**：它由编译产物提取，而技能列表是设置页里按需拉取的，
抽取时没被触发。在前端 `page-*.js` 里搜 `api/skills` 能看到三处调用。

```jsonc
// GET /api/skills?cwd=<绝对路径>
//   缺 cwd   → 400 { "error": "cwd required" }
//   cwd 越权 → 403 { "error": "Access denied" }
{
  "skills": [
    {
      "name": "demo-skill",
      "description": "…",
      "filePath": "/Users/me/.agents/skills/demo-skill/SKILL.md",
      "baseDir": "/Users/me/.agents/skills/demo-skill",
      "disableModelInvocation": false,
      "sourceInfo": { "source": "user", "scope": "user" }   // 项目技能为 project/project
    }
  ],
  "diagnostics": [
    { "type": "warning", "message": "…", "path": "…", "source": "user" }
  ],
  "projectResourcesLoaded": true,   // false → 前端禁用「Project」作用域按钮
  "cacheHit": false                 // 仅 ?cache=1 且命中缓存时 pi 才置 true
}
```

- 顺序由前端自己排（`disableModelInvocation` 升序 → 名字），后端不必保证。
- 同名技能：**项目与用户之间合并**（同名也各留一条，项目在前）；**用户目录之间按名字去重**
  （`~/.agents/skills` 与 `~/.pi/agent/skills` 常有同一份副本）。pi 是全局按 name 去重，
  项目版本会把用户版本顶掉 —— 这里刻意不同。
- `sourceInfo` 决定前端的组：`user` → Global，`project` → Project；两者都没有时落到 Other。
- `SKILL.md` **缺 `description`** 会被直接丢弃且**不产生诊断**（pi 与 toiler 一致），
  “技能写好了却不显示”先查 frontmatter（如新建的 `k8s-deploy` 曾因此消失）。

### 技能目录（决定「不同项目不同技能」）

| 来源 | 扫描范围 | `scope` |
|---|---|---|
| 项目 | **`$project/.agents/skills` 一层**。`$project` = 从 `cwd` 向上最近的 git 仓库根；不在 git 里就是 `cwd` 本身（跳过 `~/.agents/skills`） | `project` |
| 用户 | `~/.agents/skills`（跨工具） | `user` |
| 用户 | `~/.pi/agent/skills`（pi 的 agent 目录，迁移用户默认就在这） | `user` |
| 用户 | `~/.agents/toiler/skills`（toiler 自己的 agent 目录） | `user` |

两条前提，对应 pi 的 `collectAncestorAgentsSkillDirs` + `projectTrusted`：

1. **只认项目根**：约定位置就是 `$project/.agents/skills`。子目录里开会话
   （`$project/frontend`）仍读项目根那一份；子目录自己的 `.agents/skills` 不算。
   与 pi 的差异：pi 在找不到 git 根时会一路扫到文件系统根（`~/Development/.agents/skills`
   也会被当成项目技能），toiler 只认项目根，行为更好预测。
2. **项目必须已信任**：未信任时项目技能**完全不加载**，同时 `projectResourcesLoaded=false`
   （前端据此禁用「Project」作用域按钮）。否则一个刚 clone 的仓库可以靠 `.agents/skills` 里的
   提示词影响 agent。

目录不存在不产生诊断（pi 会为它报 `list_failed`，toiler 直接跳过——「没装过项目技能」是常态）。

pi 另有三个写侧/市场接口，toiler 都返回 `501 {"error":"…中文说明…"}`：

| 方法 | pi 行为 |
|---|---|
| `POST /api/skills/search` | 技能市场搜索：`{query}` → `{results}` |
| `POST /api/skills/install` | 安装技能包：`{package, scope, cwd}` |
| `PATCH /api/skills` | 改写 `SKILL.md` 的 `disable-model-invocation`：`{filePath, disableModelInvocation}` → `{success:true}` |

## GET /api/models

（见 `tools/pi-contract/raw/models.js`；字段名与 pi 的 `Mx()` 逐字对齐。）

```jsonc
{
  "models": { "<provider>:<id>": "显示名" },        // 字典，不是数组
  "modelList": [{ "provider": "p", "id": "m", "name": "M" }],
  "defaultModel": { "provider": "p", "modelId": "m" } | null,
  "thinkingLevels": { "<provider>:<id>": ["off", "low", "high"] },
  "thinkingLevelMaps": { "<provider>:<id>": { "max": "max", "minimal": null } },
  "thinkingLevelPins": { "<provider>/<id>": "level" },
  "source": "/home/u/.pi/agent/models.json" | null
}
```

### 两种分隔符不能统一

| 字段 | 键格式 | 谁在用 |
|---|---|---|
| `models` / `thinkingLevels` / `thinkingLevelMaps` | `provider:id`（**冒号**） | 模型下拉框、推理强度下拉框 |
| `thinkingLevelPins` | `provider/id`（**斜杠**） | 新会话的默认思考等级 |

### `thinkingLevels` 的取值规则

复刻 pi 的 `getSupportedThinkingLevels`（`pi-ai/dist/models.js:693`）：

1. `reasoning == false` → 只有 `["off"]`。
2. 否则从 `["off","minimal","low","medium","high"]` 起；`thinkingLevelMap`
   里值为 `null` 的等级被剔除。
3. `xhigh` / `max` 只有 `thinkingLevelMap` 里**存在且非 null** 时才追加。

> 不能用 rpi 的 `Model::supported_thinking_levels()` 代替：它对非推理模型仍返回
> `off…high`，前端会在不支持推理的模型上显示出推理强度菜单。

### 可见范围

按 `settings.json` 的 `enabledModels` 白名单过滤（§`settings::visible_models`）。
`enabledModels` 里写成 `provider/id:level` 的条目会额外进 `thinkingLevelPins`。

### 历史故障：`models` 曾经是数组

阶段 2 实现返回的是 `{models: [{…}], default: {…}, source}`，缺少
`modelList` / `defaultModel` / `thinkingLevels*`。前端把 `models` 当
`Record<string,string>` 用，拿到数组后 `modelId` 退化成数组下标 `"0"`、
`name` 变成整个对象，`Intl.Collator.compare()` 收到对象抛 TypeError，React
树被卸载 → 点「选择模型」时整页变成 Next 的 *This page couldn't load*。

回归用例：`tests/regression.rs::models_dict_and_list_share_the_same_keys`、
`tests/regression.rs::thinking_levels_match_pi_semantics`。

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

## 实现进度（`crates/toiler-server`）

| route | 状态 | 备注 |
|---|---|---|
| `GET /api/sessions` | ✅ | 字段全对齐（含 `sessionsUpdatedAt`、30s 缓存、`?force=1`） |
| `GET /api/sessions/{id}` | ✅ | `info`/`tree`/`context`/`paging`/`totalActiveMs` 全对齐 |
| `PATCH /api/sessions/{id}` | ✅ | 追加 `session_info` 条目（pi 的做法） |
| `DELETE /api/sessions/{id}` | ✅ | 关活 agent + 删文件 |
| `GET /api/sessions/{id}/state` | ✅ | 与 `GET /api/agent/{id}` 同实现 |
| `GET /api/sessions/search` | ✅ | 元数据 + 文本块 + compaction.summary，58 字符 snippet |
| `POST /api/agent/{id}` | ✅ | `prompt`/`steer`/`follow_up`/`clear_queue`/`abort`/`get_state`/`set_model`/`navigate_tree`/`set_steering_mode`/`set_follow_up_mode`（后两个只落盘，`applied:false`，新会话生效；见 `GET/PUT /api/queue-modes`） |
| `GET /api/agent/{id}` | ✅ | `{running}` / `{running, state}` |
| `GET /api/agent/{id}/events` | ✅ | SSE，裁剪规则与 pi 逐条对齐 |
| `POST /api/agent/new` | ✅ | 建 v3 会话 + 登记项目 + 执行 body 里的命令；`{success,sessionId,data,model,thinkingLevel}`。body 的 `toolNames`（前端工具预设，默认 `read/bash/edit/write`，桌面工具始终启用）与 `uiLocale` 也在建会话时应用 |
| `GET /api/agent/running` | ✅ | `{runningSessionIds, pendingUiSessionIds}`（pi 的 `56429.am()`） |
| `GET /api/files/{*path}` | ✅ | 六种 `type` 全实现（`list`/`read`/`download`/`meta`/`preview`/`watch`），`Range` 206/416，SSE 命名事件 `connected`/`change` |
| `PUT /api/files/{*path}` | ✅ | `{content, allowEmpty}`；空覆盖 409 `empty_overwrite_blocked`；256KB 上限 |
| `PATCH /api/files/{*path}` | ✅ | 重命名；纯文件名校验；撞名 409 `already_exists` |
| `POST /api/files/{*path}` | ✅ | `?type=upload-check` / `?type=upload&conflict=error\|overwrite\|skip`；25MB 单文件 / 100MB 总量；部分成功 207 |
| `GET /api/models` | ✅ | `{models, modelList, defaultModel, thinkingLevels, thinkingLevelMaps, thinkingLevelPins, source}` 全对齐；`models` 是 `provider:id` 字典 |
| `GET /api/skills` | ⚠️ | 项目技能：`$project/.agents/skills`（项目根 = 最近 git 根，需已信任）；用户技能：`~/.agents/skills` + `~/.pi/agent/skills` + `~/.agents/toiler/skills`，三者按名字去重；两组合并返回（同名不互相顶掉）。pi 的 `skillPaths`/package 技能没有（无 package 系统），`usageCount` 与缓存也不提供（`cacheHit` 恒 false） |
| `POST /api/skills/search` | ❌ | 501：技能市场（PLAN 已砍） |
| `POST /api/skills/install` | ❌ | 501：技能市场（PLAN 已砍） |
| `PATCH /api/skills` | ❌ | 501：会改写用户目录的 `SKILL.md`，写侧暂不开放 |
| `GET /api/mobile/state` | ✅ | 手机投影（`mobile_view`）：图片转 `[N images]` 占位、思考/工具入参截断、默认 30 条窗口 + 40 条内保留细节；实测 474KB → 54KB |
| `GET /api/mobile/access` | ✅ | 读 `desktop-access.json` + 代理运行态 + 隧道运行态；`publicHostnameDisplay` 转 Unicode |
| `PUT /api/mobile/access` | ✅ | 密码 ≥8 位、named 模式校域名、公网入口要求先有密码；0600 原子写、字段级合并 |
| `GET /api/mobile/pair` | ✅ | 五种 `reason`；`lanPort` 带 `ownerPid` 校验；`publicUrl` 用 Unicode 域名；**不含凭据** |
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
| `GET /api/auth/providers` | ✅ | **OAuth / 订阅**列表：`{id,name,baseUrl?,usesCallbackServer,loggedIn,supportsApiKey}`，11 家（含 `qoder-cn`，见差异 6） |
| `GET /api/auth/all-providers` | ✅ | **API Key** 列表：`{id,displayName,baseUrl?,configured,source?,modelCount,supportsOAuth}`，42 家（含 `apisets`） |
| `GET /api/auth/api-key/{provider}` | ✅ | `{provider,displayName,configured,source,models,maskedKey}`，明文 key 永不出站 |
| `POST /api/auth/api-key/{provider}` | ✅ | `{apiKey}` → 写 `auth.json`（0600，原子替换） |
| `DELETE /api/auth/api-key/{provider}` | ✅ | 类型不符 → 409 |
| `POST /api/auth/logout/{provider}` | ✅ | 删 oauth 凭据；类型不符 → 409 |
| `POST /api/auth/login/{provider}` | ⚠️ | 501 + `{error}`：只有 `qoder-cn` 有 OAuth 登录，且走 GET（见下一行） |
| `GET /api/auth/login/{provider}` | ✅ | `text/event-stream`（前端用 `EventSource`）。`qoder-cn` 走真实的设备码登录；其它 provider 只发一条 `{type:"error",message}` 就结束 |
| `GET /api/models-config` | ✅ | 原样返回 `models.json` |
| `PUT /api/models-config` | ✅ | 保留 `$schema`、清洗无 id 的模型、`cost` 缺项补 0 |
| `DELETE /api/models-config` | ✅ | `{kind:"provider"\|"model", provider, modelId?}`，顺手清 `enabledModels` |
| `GET /api/models-config/enabled?provider=` | ✅ | `{provider,models[],enabledModels,unscoped,warnings:string[],refreshing}`；`warnings` 是**字符串数组**（pi 取 `resolveModelScopeWithDiagnostics` 的 `diagnostics.map(d => d.message)`），前端逐条当文本渲染，给对象会整页崩。`apisets`（Coding）的清单来自远端目录缓存，空列表时会顺手发起一次后台刷新并回 `refreshing:true`（见差异 21） |
| `PUT /api/models-config/enabled` | ✅ | `{provider,modelIds}` → 折叠成 `provider/*` 或逐条写 `settings.json` |
| `POST /api/models-config/discover` | ✅ | 打上游 `/models` 并按 pi 的字段兼容表解析 → `{models,endpoint}` |
| `POST /api/models-config/test` | ✅ | 真跑一次补全 → `{ok,latencyMs,status,responseText}` |
| `GET /api/models-config/catalog` | ⚠️ | models.dev 目录已砍，返回空表 + `note` |
| `GET /api/git?cwd=` | ✅ | 分支/上游/增删/未跟踪/凭据类型/进行中的操作；`tags` 是 toiler 追加的字段（见差异 22） |
| `POST /api/git` | ✅ | pi 的 20 个动作（含 `merge_branch`、`*_remote_branch`、`*_branch`）+ toiler 追加的 `create_tag` / `delete_tag` / `push_tag` / `push_tags`（见差异 22、23），返回操作后的状态 |
| `GET /api/git/status?cwd=` | ✅ | `{isGitRepository,repositoryRoot,files[],additions,deletions}` |
| `GET /api/git/diff?cwd=&path=` | ✅ | `{supported,status?,patch?}`；未跟踪文件会合成补丁 |
| `POST /api/git/commit-message` | ✅ | NDJSON：`update` → `done`（失败 `error`）；用 `settings.json` 的默认模型 |
| `GET/POST/DELETE /api/worktrees` | ✅ | 列出 / 新建（`action:"checkout"` 走切分支）/ 删除（脏工作区 409 + `dirty:true`） |
| `GET/PUT/PATCH/DELETE /api/projects` | ✅ | 用 toiler 自己的 `<appDir>/work.sqlite`（`~/.agents/toiler/work.sqlite`）；接口形状与 pi 一致，但**不再共库** |
| `GET/POST /api/project-trust` | ✅ | `{requiresTrust,trusted}`；`trust.json` 与 pi 同格式 |
| `GET /api/plugins?cwd=` | ⚠️ | 只读：列 `settings.json` 的 `packages` + 安装状态 + 包内资源；安装根只认 toiler 自己的 agent 目录（见差异 19） |
| `POST /api/plugins` | ⚠️ | `install`/`remove`/`update`/`disable`/`enable` 全支持：npm 走本机 npm（`--legacy-peer-deps`，安装根 `<toilerHome>/plugins/npm`），启停改 `settings.json`；**git: 来源不支持**；找不到 npm → 500 + 中文提示。注意：装上的 TS 扩展 rpi 不会加载，功能不生效 |
| `GET /api/plugins/catalog` | ⚠️ | 恒为空页（`{plugins:[],hasMore:false}`）：插件市场同技能市场，PLAN 明确不做 |
| `POST /api/plugins/catalog/translate` | ⚠️ | 恒为 `{translations:{}}`：没有目录可翻 |
| `GET /api/mcp?cwd=&scope=&includeContent=` | ⚠️ | 读配置文件 → `{path,servers}`；`includeContent=1` 再带 `content` 原文（文件不存在时是 `{\n  "mcpServers": {}\n}\n`）。global 落在 `<configs>/mcp.json`（见差异 20） |
| `PUT /api/mcp` | ⚠️ | `{cwd,scope,content}` → 校验 `mcpServers` 是对象后原子写回 → `{path,servers}`；项目作用域未信任 403 |
| `POST /api/mcp` | ⚠️ | `{cwd,scope,presetId}` → 把内置 preset 的 `entry` 写进 `mcpServers` → `{path,serverName,added,servers}`；未知 preset 404 |
| `POST /api/mcp/test` | ⚠️ | 探活：`url` 发一次 `initialize`（10s）→ `passed`/`auth-required`/`failed`/`unknown`；`command` 跑 `--version` → `configured`/`failed`。**不建真实 MCP 会话** |
| `GET/PUT /api/tools/settings` | ⚠️ | `{isWindows:false,powerShellEnabled:false}`；PUT 501（shell 工具是 bash，没有 PowerShell 通道） |
| `GET/PUT /api/queue-modes` | ⚠️ | `{steeringMode, followUpMode}`（`all` / `one-at-a-time`，缺省 `one-at-a-time`），读写 `settings.json` 同名键；非法值 / 空 body 400。差异：pi 的 `AgentSession` 能改内存里的队列模式，rpi 的在 `AgentHarness::create` 时就定下来（`AgentHarnessOptions::steering_mode` / `follow_up_mode`），没有运行期 setter，所以 `PUT` 与 `set_steering_mode` / `set_follow_up_mode` 命令只对**新会话**生效；`GET` 必须实现，否则常规页在「完成提示音」下面常驻一行 `Not found` |

### `/api/files` 的实现细节（阶段 5）

- **路径形状**：三种都认 —— `/api/files//Users/me/a.ts`、`/api/files/Users/me/a.ts`、
  `/api/files`（空 → 默认 cwd）。解析**只有一种解释**：去前导斜杠后从根拼回去，
  不再猜「相对 cwd」，否则不存在的路径会被解析到别处、把 404 变成 403。
- **授权**：两层 —— 词法包含（`within_roots`）+ realpath 包含（`within_roots_real`）。
  另外接受第二条通道：`?sessionId=` 对应的会话正文里提到过这个绝对路径（pi 的
  `isFilePathReferencedBySession`，边界判定含 `a.rs:12:5` 这类行列号）。
- **DOCX 预览**：自己解 OOXML（zip 中央目录 + `word/document.xml`），支持标题 / 粗体 /
  斜体 / 下划线 / 列表 / 表格 / 实体；**不做**图片内联（输出 `[图片]` 占位）、样式表、
  页眉页脚、脚注、公式、修订痕迹 —— 目标是「打开就能读」，不是像素级还原。
- **`type=watch`** 的变更签名是 `mtime + ctime + inode + size`（与 pi 同组字段），
  只看 mtime 会被一次读取（刷新 atime）触发自激刷新。

### 与 pi 的已知差异

> 标 ⚠️ 的行是**有意保留的差异**（多为 PLAN「明确不做的」里的取舍），不是欠账；
> 阶段 5 回归时逐条确认过一遍，能补的都补了（`files` 六种 type、`deferThinking`、
> 手机投影等），补不了的写在这里并说明原因。

1. ~~**`projectRoot` 不做 git 探测**~~ —— 阶段 3 已修：`projectRoot` 走
   `git rev-parse`，worktree 时取主仓库根并带 `worktreeBranch`（60s 缓存）。
2. **`models` 元数据不全**：只给前端首屏需要的字段，没有价格/能力目录。
3. **`deferThinking`** ✅ 阶段 5 已实现：思考正文清空 + 标 `deferred:true`（前端据此显示
   「展开思考」，点开走 `/api/sessions/{id}/entries/{entryId}/thinking`）。
   **`deferMedia`** ✅ 已实现：`content` 里的图片块换成一行
   `[image: <mime>, ~<KB> KB]` 文本块（嵌套 `source` 与扁平 `data`/`mimeType` 两种形状都认）。
   前端首屏一直带 `deferMedia=1`，之前是空实现，带不带返回的字节完全一样——
   一条有图的会话首帧能到 1.38MB，实现后降到 214KB（实测同一条会话）。
   注意：占位只发生在 `GET /api/sessions/{id}` 的 `context.messages` 上，
   `/api/sessions/{id}/entries/...` 取单条、导出、手机投影一律返回原字节。
4. **tree 的 `compressedEntryIds`** 只在深度超过 200 时出现（与 pi 的代码路径一致，但 pi 的 `visible` 集合还有别的分支，未 100% 验证）。
5. **自定义 role 的消息**：写 v3 时降级为 `role:"custom"` + `customType`（pi 认的集合有限）。
6. **auth provider 列表来自写死的内置表**：pi 的列表来自 provider 目录
   —— `@earendil-works/pi-ai` 的 `builtinProviders()`（1.4.0 版 42 家）**加上
   pi-desktop 自己注册的 2 家**（`apisets` / `qoder-cn`，见 `lib/desktop-providers.ts`）。
   rpi 只有三种 `api` 协议的实现、没有目录接口，所以 toiler 把这份目录**写死**在
   `crates/toiler-server/src/builtin_providers.rs`（`PROVIDERS` + `DESKTOP_PROVIDERS`，
   每家含 id/name/baseUrl/是否 API Key/OAuth 名/chat 模型 id）。
   文件头注释里有重新提取的脚本。

   | 列表 | pi 的行为 | toiler |
   |---|---|---|
   | `/api/auth/providers`（订阅区） | `buildOAuthProviderList`：声明了 OAuth 的 10 家 | 一致（含 `anthropic`、`qoder-cn`） |
   | `/api/auth/all-providers`（API Key 区） | `buildApiKeyProviderList`：声明了 API Key 登录的 42 家，跳过 `source` 是 `models_json_key`/`models_json_command` 的 | 一致 |

   改名历史：阶段 5 之前这两个 route **语义是反的**（`providers` 返回 API Key 形状、
   `all-providers` 恒空），前端「添加模型 → 供应商」面板因此只剩一张自定义卡片。
   `modelCount` 取内置目录的 chat 模型数，加上 `models.json` 同名 provider 的模型并去重。

   仍然缺的部分：
   - **内置模型目录没带过来**：`/api/models` 依旧只读 `models.json` + 内置静态目录
     （`qoder-cn` 的快照、`apisets` 的远端缓存，见差异 21），pi 目录里其余 40 家的
     模型清单没搬，所以选中那些供应商、填了 API Key 之后，还得自己把模型 id 写进
     `models.json`。
   - **OAuth 登录**（差异 7）：`/api/auth/providers` 里那 10 家照样列出来。
     `qoder-cn` 可以真的登录（设备码 + COSY 签名全部移植，模型能直接对话）；
     其余 9 家点「Login」只会拿到一条「OAuth 登录未实现」的 SSE 错误，其中 8 家是
     双通道，可以改走「API Key」区配置，只有 `openai-codex` 完全没有可用通道。
   - **`api` 协议支持面**：pi 目录里 `bedrock-converse-stream`、`google-generative-ai`、
     `google-vertex`、`azure-openai-responses`、`mistral-conversations`、
     `openai-codex-responses`、`typesafe-system-one`、`pi-messages`、`openrouter-images`
     这几类 rpi 没有实现（见 `catalog.rs` 的映射表），供应商能列出来但跑不通。
7. **OAuth 登录只有 `qoder-cn` 是真的**：`GET /api/auth/login/qoder-cn` 跑完整的设备码
   + PKCE S256 流程（`pi-desktop/lib/qoder-cn/auth.ts` 的移植），成功后把 oauth 凭据
   写进 `auth.json`（`{type:"oauth", access, refresh, expires, uid, nickname, userType,
   organizationId, organizationName}`），前端据此把卡片标成已连接。
   其余 10 家仍是「不支持」：`POST` 回 501，`GET` 回一条
   `{type:"error",message:"OAuth 登录未实现…"}` 的 SSE 事件（前端只认这个形状，
   回 501/405 会显示英文「Connection lost」）。
8. **`qoder-cn` 的本机转发端点不是 `/api` 契约**：
   `POST /qoder-cn/{token}/v1/chat/completions`。rpi 的 provider 不允许注入传输层，
   所以把模型的 `baseUrl` 指到本进程，由它做 COSY 签名 + 请求体改写 + SSE 信封解包
   （等价于 pi 的 `withQoderCnFetch`）。路径令牌是进程启动时的随机串，且只收
   loopback 对端 —— 这条端点会带着账号凭据刷上游额度。
   与 pi 的差异：模型目录用内置静态快照（pi 登录后还会拉一次官方 `model/list`）；
   `user/plan`、`quota/usage` 这些用量接口没移植。
9. **`/api/auth/api-key/{provider}` 的 `displayName`** 取 `models.json` 的 `name`，缺省用 id
   （pi 取 models.dev 的展示名）。
10. **git 凭据**：pi 用 AES-256-GCM 加密存进 `git-credentials.json`（密钥在 macOS Keychain）。
   toiler 把凭据交给系统 credential helper（`git credential approve`），因此
   `hasSavedCredential` / `savedCredentialUsername` 恒为 `false` / `null`；
   「记住凭据」对后续 `git` 命令仍然生效。
11. **`/api/git/commit-message` 的流**：pi 每收到一段就发一行 `update`；toiler 攒完再一次
    `update` + `done`（字段相同，少了中间增量）。
12. **`/api/sessions/{id}/export` 的 HTML**：响应头与文件名规则与 pi 一致，但内容是自己渲染的
    线性转写稿（pi 用 `pi` CLI 生成带树状导航的 HTML）。`?inline=1` / `?desktop=1` / `returnTo`
    都支持。
13. **`/api/agent/{id}/bash-output` 的会话归属校验**：pi 只认
    `role:"bashExecution"` 的顶层 `fullOutputPath`；toiler 额外接受新工具形态
    （`role:"toolResult"` 的 `details.fullOutputPath`），因为当前 pi 就是这么写的。
13. **`/api/models-config/catalog`** 恒为空表（models.dev 目录已砍，见 PLAN「明确不做的」）。
14. **`/api/agent/running/events` 的 `pendingUiSessionIds`**：`ask_user` 卡在提问上的
    会话 id（与 pi 同义）。`/api/mobile/state` 的 `pendingUiRequest` 也从同一处取第一个
    阻塞式请求（`select`/`confirm`/`input`/`editor`），手机端据此弹卡片。
15. **`/api/mobile/access` 的 `publicHostnameDetected`** 恒为 `null`：pi 用 `cert.pem`
    里的 Cloudflare API token 去查 DNS 反推域名，toiler 刻意不做（不把用户的 token
    发到公网）。设置页里手填一次域名即可；`publicHostnameDisplay` 仍会正确转成 Unicode。
16. **`/api/mobile/pair` 的 `publicUrl` 来源**：pi 会抓 quick 模式的 `trycloudflare`
    日志；toiler 的隧道管理器把地址写进 `desktop-access.tunnel.json`，这里直接读。
17. **`/api/sessions/{id}` 的 context 会归一化 `toolCall` 块**：v3 文件里是
    `{id,name,arguments}`，接口输出 `{toolCallId,toolName,input}` —— **必须**这么做，
    否则前端 `isFileWritingToolName(undefined)` 直接 TypeError，打开任何带工具调用的
    历史会话都整页白屏（阶段 5 走查实测）。
18. **`role: "system"` 的转录消息不进 context**（与 pi 的 `entryToUiMessage` 一致）：
    pi ≥0.86 把 prompt / 工具清单写成 system 消息，它们是 provider 输入而不是对话。
19. **`/api/plugins` 只读，且只认 toiler 自己的安装目录**：pi 的插件面板背后是
    `DefaultPackageManager`（npm/git 安装 + 启停 + 资源池合并）；toiler 只做投影：
    读 `settings.json` 的 `packages`，按 `<agentDir>/npm/node_modules/<name>`、
    `<agentDir>/git/<host>/<path>`（项目作用域换成 `<cwd>/.pi/…`）判断装没装。
    **不**回退 `~/.pi/agent/npm`，也不跑 `npm root -g` / pnpm —— 因为 toiler 的
    agent 目录是自己的（`~/.agents/toiler`），把 pi 那边装的包显示成「已安装」会误导：
    rpi 并不会加载它们。资源发现按 `package.json` 的 `pi` 清单（存在则只按清单字段收，
    缺失的类型不再回退约定目录），无清单时扫 `extensions/` `skills/` `prompts/` `themes/`；
    不做 `.gitignore` 过滤、不展开清单里的 glob、对象形态的过滤清单只认「空数组 = 该类型
    全禁用」。`totals` 只统计包内资源（不含 `settings.json` 顶层条目与自动发现目录）。
    少这条路由时插件页会拿到静态兜底的 404 `{"error":"Not found"}`，并把它当成
    一条插件画出来——这就是「不显示已安装插件」的直接原因。
20. **`/api/mcp` 只读写配置，不加载服务器**：pi 的 MCP 面板背后是
    `pi-mcp-adapter` 扩展（按配置起 stdio 进程或连 SSE，工具进会话）；toiler 只做
    文件那一半——`GET`/`PUT`/`POST` 都是读写 `mcp.json`，`/api/mcp/test` 只探端点
    （有 `url` 发一次 `initialize`，有 `command` 跑 `--version`）。**rpi 没有 MCP 客户端，
    服务器不会真的连上，工具也不会出现在会话里**。另外 global 那份配置文件落在
    `<configs>/mcp.json`（`~/.agents/toiler/configs/mcp.json`，与 `models.json` 同目录），
    而不是 pi 的 `~/.pi/agent/mcp.json`。缺这组路由时 MCP 页会落到静态兜底拿
    404 `{"error":"Not found"}`，页面上直接显示成红字。
21. **`apisets`（面板里叫 Coding）的模型目录是远端拉取 + 本地缓存**：pi 在
    `lib/desktop-providers.ts` 里给它 `registerProvider` 了一个 `refreshModels`，
    打上游 `GET https://coding.apisets.com/v1/models` 并持久化到
    `models-store.json`；toiler 的等价实现是 `crates/toiler-server/src/apisets.rs`：

    | 环节 | pi | toiler |
    |---|---|---|
    | 缓存文件 | `<agentDir>/models-store.json` | `<configs>/models-store.json`（与 `models.json` 同目录，格式逐字一致） |
    | 格式版本 | `etag: "apisets-models-api-v2"` | 同名常量，对不上就整份作废 |
    | 有效期 | 30 分钟 | `CACHE_TTL`（30 分钟；过期仍可离线用缓存） |
    | 拉取条件 | `credential.type === "api_key"` 且有 key | 同左（没 key 时保持旧缓存，不清空） |
    | 协议 | 每条模型自带 `api`（`gpt-*` → Responses，其余 → Anthropic Messages） | 同左；rpi 只按 `model.provider` 解析 provider，所以用 `ApiSetsProvider` 按 `model.api` 分发 |

    `GET /api/models-config/enabled?provider=apisets` 在列表为空时会发起一次后台刷新并
    回 `refreshing:true`（前端隔 1.5s 重试两次 / 用户点「重试」）；另外
    `POST/DELETE /api/auth/api-key/apisets` 之后会重建内置目录。
    **还没做的**：`deepseek` / `openrouter` 两家在 pi 侧同样有 `refreshModels`
    （走 models.dev 目录），toiler 仍只读 `models.json`。

22. **`/api/git` 追加了本地标签管理（pi 没有）**：`ACTIONS` 从 20 条扩到 22 条，
    多出 `create_tag` / `delete_tag`，`GET /api/git` 的返回体也多一个 `tags: string[]`
    （本地 tag 名，`git for-each-ref refs/tags` 的短名，升序去重，纯本地不区分是否已推送）。
    pi 的 Git 面板只做分支，没有标签入口，所以这是扩展而不是逆向结果；面板上的
    「标签」区块做列表 / 创建 / 删除本地标签，推到远端见差异 23。

    | 动作 | 入参 | 等价命令 |
    |---|---|---|
    | `create_tag` | `tag`（必填）、`message`（可选，给了就建附注 tag）、`target`（可选，缺省 HEAD） | `git tag [-a <tag> -m <message>] <tag> [<target>]` |
    | `delete_tag` | `tag`（必填） | `git tag -d <tag>`（只删本地） |

    名字校验走 `git check-ref-format refs/tags/<name>`（与分支的 `--branch` 校验同一思路），
    重名 / 非法名 / 删不存在的 tag 都由 git 自己报错并原样透出，不静默成功。

23. **`/api/git` 追加了标签推送（pi 没有）**：`ACTIONS` 再扩到 24 条，多出
    `push_tag` / `push_tags`。两者与 `fetch` / `pull` / `push` 共用同一条网络链路
    （`run_remote_action`）：读 `origin` 的 URL 判断凭据类型（`credentialKind`）、
    按需注入 `credential`（HTTPS token / SSH passphrase 走 askpass 临时文件）、
    成功后按 `rememberCredential` 交给系统 credential helper。**只推 `origin`**，
    与 `set_remote_url`、`remote` 字段的既有假设一致。

    | 动作 | 入参 | 等价命令 |
    |---|---|---|
    | `push_tag` | `tag`（必填，本地必须已存在） | `git push origin refs/tags/<tag>` |
    | `push_tags` | 无 | `git push origin --tags` |

    标签不存在时本地就报 `Tag not found: <tag>`，不把 git 的
    `src refspec … does not match any` 原文抛给用户。`push_tags` 沿用 `--tags` 的
    语义：远端已有同名 tag 且指向不同提交时 git 会拒绝整条命令，错误原文原样透出
    （面板顶部的红条），**不会**静默覆盖远端 tag；要覆盖得先删远端 tag（当前面板
    不提供，`delete_tag` 只删本地）。

    响应与其它动作一致：返回操作后的完整仓库状态，所以推送后标签列表与 `tags`
    字段不变（本地标签没变），面板上看不到「已推送」标记——这与 `push` / `fetch`
    一样没有成功提示，只有失败才出红条。

### 目录与文件

两层，互不混：

```text
~/.agents/                       ← 通用根（多个 agent 工具都认）
├── skills/                      各工具共用
├── mcp/
├── commands/
├── tools/
└── toiler/                     ← toiler 专属
    ├── configs/                 auth.json / models.json / settings.json /
    │                            trust.json / desktop-access*.json /
    │                            models-store.json（apisets 远端目录缓存）
    ├── sessions/                v3 会话（`--<cwd>--/<ISO>_<id>.jsonl`）
    ├── plugins/                 （预留）
    └── work.sqlite              项目列表 + 选中项
```

| 环境变量 | 作用 |
|---|---|
| `AGENTS_HOME` | 改通用根（默认 `~/.agents`） |
| `TOILER_HOME` | 改 toiler 专属目录（默认 `<agentsHome>/toiler`） |
| `TOILER_AGENT_DIR` | 旧名兼容，等价于 `TOILER_HOME` |
| `TOILER_SESSIONS_ROOT` / `TOILER_MODELS` | 单独覆盖会话根 / 模型目录 |
| `PI_CODING_AGENT_DIR` | 只影响**播种源**（`legacy_pi_agent_dir`） |

**为什么不继续用 `~/.pi/agent`**：那是 pi 的目录，pi 会按自己的节奏迁移 schema、
清理缓存、重写文件。toiler 的配置放进去会跟着被改，而且两边谁改了哪个键说不清。

**首次启动的种子数据**：`configs/` 为空时，从 `~/.pi/agent` **复制**
`auth.json` / `models.json` / `settings.json` / `trust.json` / `desktop-access*.json`
（0600 落盘）与会话文件；**旧文件保留不删**（用户可能还在用 pi）。
一旦播过就不再动 —— 否则用户后来删掉的凭据会「复活」。
会话单独判定：配置播过之后新增的会话仍会补搬，已存在的不覆盖。

**项目库**：`work.sqlite` 首次创建时，从旧的 `<piAgentDir>/pi.sqlite` **只读**
导入 `added_projects` + `last_selected_project`；库已存在就不再导入。

### rpi 侧发现的已知问题（不属本仓库职责）

- **`openai-completions` 流式 toolCall 的 `id` 会重复拼接**（qoder 端点每段 delta 都发完整 id，
  rpi 做的是 `concat` 而不是 `set`）。实测 4 段 → 280 字符。
  影响：provider 通信正常、v3 文件可读，但 id 是脏的。如需修复要改 rpi 依赖或在服务层后处理。
