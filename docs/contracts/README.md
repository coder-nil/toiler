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

排序：`modified` 倒序（字符串比较）。会话列表和搜索不显示无消息、无名称且不是分叉的草稿；
旧文件不删除，仍可通过 id 读取。首条消息写入 v3 文件后使列表缓存失效，并在会话 SSE 上发送
`{"type":"session_published"}`，让侧栏无需等待模型回复即可刷新。

## GET /api/sessions/[id]

查询参数：`deferThinking`、`deferMedia`（出现即 true）、`limit`（默认 120，夹在 1..400）。

`GET /api/sessions/{id}/context` 认同一组 `deferThinking` / `deferMedia`（见下）。

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

`prompt` / `steer` / `follow_up` 的图片附件在**平级的 `images` 字段**里，不在 `message` 里：

```jsonc
{"type":"prompt", "message":"看这张图",
 "images":[{"type":"image", "data":"<base64>", "mimeType":"image/png"}]}
```

服务端把它们拼成 user 消息的内容块（文本块在前、图片块在后；只有图没有文字时不塞空
文本块），图片因此能落进 v3 文件、也能进模型上下文。`images` 缺失或为 `null` 时走原
文本路径（裸字符串）。单张上限 10MB、每次最多 10 张（前端 `image-attachments.ts`
的 `validateAgentImages` 先拦一道）；服务端遇到不合法的附件会**报错**而不是静默丢掉。

`clear_queue` 清空两条队列并返回**被清掉的文本**（`{steering:[…], followUp:[…]}`），
前端的「撤回排队」按钮拿它回填输入框；空队列返回两个空数组（不是 `null`）。

`set_model` 与 pi 一致：按 `provider/modelId` 查模型目录，追加一条 `model_change`
到 v3 会话文件（前端切完的 `loadSession` 靠它回显模型），返回 `{id, provider}`；
查不到时 `500 {"error":"Model not found: <provider>/<modelId>"}`。

用户成功执行 `set_model`，或通过 `/api/agent/new` 显式指定模型后，会把
`settings.json` 的 `defaultProvider` / `defaultModel` 更新为最近选择，供后续新会话
使用并在重启后保留；打开历史会话不更新偏好。尚未创建会话的草稿选择由浏览器记住，
只在模型仍处于可见列表时恢复，并作为显式模型传给建会话请求。显式模型已不在目录时，
`/api/agent/new` 返回 `400`，不建会话，也不换用默认模型代答。

续聊优先恢复会话当前分支最后一次有效的 `model_change`，没有时使用最后一条助手消息的
模型标记。历史模型不在运行目录时才用默认模型；不带 `leafId` 的 `/api/sessions/{id}`
以同一规则返回 `context.model`（活会话直接返回正在使用的模型），旧消息的模型标记不变。

`set_thinking_level`（`{"type":"set_thinking_level","level":"high"}`）会实际更新 lane，
向 v3 文件追加 `thinking_level_change`，返回 `{thinkingLevel}`（当前模型实际生效的级别）。
`level` 支持 `off/minimal/low/medium/high/xhigh/max`，其它值报错且不更新偏好。
同时将用户选择保存为 `settings.json.defaultThinkingLevel`，新会话与重启后沿用；
`/api/agent/new` 的 `thinkingLevel` 在第一轮 prompt 前应用。历史会话优先恢复自己的
`thinking_level_change`，不覆盖全局偏好。模型不支持的级别在运行时回落到
`off`；不支持的级别仍保留原始偏好，切回支持的模型后可恢复。草稿里的选择由浏览器
暂存，并与建会话请求一起发送。

**`auto` 已从等级表删除（兼容别名仍在）。** 界面过去有第八个选项 `auto`（「默认推理」），
但 rpi 的等级枚举里没有它，`effective_thinking_level` 必然把它落成 `off`；两者运行时
完全等价，只让用户以为「选了什么都没发生」。现在：

- `GET /api/models` 的 `thinkingLevels` 与前端菜单都只有 `off…max` 七档；
  缺省（从未选择过）使用 `medium`（`settings::DEFAULT_THINKING_LEVEL`）。
- 旧客户端发来 `{"level":"auto"}` 时按 `medium` 生效并落盘，`settings.json` 里不会
  再写出 `auto`；`settings.json.defaultThinkingLevel` 里遗留的 `auto` 读时被忽略，
  等同「没选过」。
- `enabledModels` 的 `provider/id:level` 后缀本来就只认真实档位，`auto` 不会被当成
  等级后缀，因此也不存在对应的 `thinkingLevelPins` 条目。

历史背景（已不成立，保留供追溯）：`/api/agent/new`、`get_state`、
`/api/sessions/{id}` 的 `context.thinkingLevel` 以及 v3 文件里的 `thinking_level_change`
回报的一直是「生效值」而非用户选的那一档；`auto` 存在时它与 `off` 在生效值层面
不可区分，前端只能靠 `resolveDisplayThinkingLevel` 把 `off` 还原成 `auto`（
`scripts/thinking-level-preference.test.mjs` 守着这条）。删除 `auto` 后显示值与生效值
同源，这个还原只剩「模型不支持所选档位时以生效值为准」这一条语义。

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
`preamble → <tools> → <rules> → <skills> → <project_context> → <cwd>` 分节
（`system_prompt.rs`），与 pi 的 `effectiveSystemPrompt()` 同义。缺了它前端面板
会一直显示「系统提示词尚未加载」，所以它是契约的一部分，不是可选字段。

`<skills>` 段的内容与 `GET /api/skills` **同源**（`crate::skills::load`），
每条是「名称: 说明」加一行「文件：<SKILL.md 绝对路径>」。两条门控：

- 没有 `read` 工具时整节不出现（技能正文只能靠 `read` 打开）；
- `disable-model-invocation: true` 的技能不列（面板里仍可见、仍可 `/skill:名称` 显式调用）。

与 pi 的差异：pi（及 rpi 的 `format_skills_for_system_prompt`）用英文的
`<available_skills>` 列表，toiler 按界面语言自己拼中文/英文 `<skills>` 段。
为避免出现两份清单，装进 `AgentHarnessResources.skills` 的每个技能都带
`disable_model_invocation: true`：rpi 的可见性过滤会跳过它们，但
`/skill:名称` 的展开仍能查到（`expand_skill_input` 不过滤这个标志）。

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

`set_chat_mode`（`{"type":"set_chat_mode","on":true|false,"toolNames":[…]}`）
是输入框左侧 π 图标的「工作模式 / 普通对话」开关，返回
`{chatMode, systemPrompt, toolNames}`。`on: true` 时同时做两件事：

- **停用所有工具**（可激活集合置空，注册表不动，切回去时能按预设恢复）；
- **系统提示词变空串**。空串不是「一个空的 system 消息」：`rpi_agent` 的
  `agent_loop` 对空 system_prompt 发 `None`，请求里**完全没有 system 消息**。
  普通对话模式的语义就是「不带系统提示词」，包括 `<project_context>`
  （AGENTS.md）与末尾的语言指令。

`on: false` 时按请求体里的 `toolNames` 恢复工具集（前端带的是进普通对话模式前的
预设，因为这份记忆只存在浏览器的 `localStorage`）并重算提示词。

`get_state` 同时回报 `chatMode`，前端重开面板时靠它校正界面状态，不能只信
`localStorage`。注意与「工具预设全关」的区别：后者只是工具为空，preamble、
行为准则与 `<cwd>` 仍会发出去，`systemPrompt` 是非空字符串。

`extension_ui_response`（`{"type":"extension_ui_response","id":…,"value"|"confirmed"|"cancelled":true}`）
回答 `ask_user` 的提问，返回 `null`；请求已作废（超时/中止）时同样返回 `null`
（前端只是晚了一步，不算错误）。Toiler 为多题问答增加 `{id, action:"back", value?:"<当前输入草稿>"}`
响应：回到上一题，不提交当前草稿，也不重置整个问卷的超时预算；重新作答会覆盖旧答案。
`extension_ui_input`（custom UI 的逐键输入）
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
     `partial.content[contentIndex]`）。**只在非空时附**：`openai-completions`
     在 `toolcall_start` 那一刻名字还是空串（`Content::tool_call("", "", {})`，
     名字要等后续 delta 才拼进来）。Responses 通常在 `output_item.added` 已给出
     名字，不必等到 `output_item.done`。
     发包空串会踩住前端 `event.toolName ?? current.toolName` 的兜底
     （只在 `undefined` 时保留旧值），把已经学到的名字覆盖成空。
   - `tool_execution_update`：只留 `{type, toolCallId, toolName, partialResult}`
   - `agent_end`：只留 `{type}`
3. 扩展 UI 请求：`{"type":"extension_ui_request","id":…,"method":"select"|"confirm"|"input"|"editor",…}`，
   请求作废（回答/超时/中止）时补一帧 `{"type":"extension_ui_resolved","id":…}`；
   `ask_user` 额外携带 `canGoBack:boolean`（首题为 false）与可选的
   `initialAnswer:string|boolean`（上次作答或返回时暂存的输入），供桌面/手机恢复答案；
   返回上一题也先作废旧请求，再用新 id 发送目标题，快照回放沿用同一完整请求帧。
   `method` 还有 `setStatus` / `setWidget`（`task_progress` 的展示，非阻塞）。
3.5 排队变化：`{"type":"queue_update","steering":["<文本>"],"followUp":[]}`。
   入队（`prompt` 运行中 / `steer` / `follow_up`）、排队消息投递、清空（`clear_queue` / `abort`）
   与整轮运行结束时推送剩余队列。排队消息落盘后立即推送，不等它的模型回复；
   桌面端据此刷新排队条，手机端收到后重拉一次快照。
3.6 重试提示：`{"type":"auto_retry_start","attempt":1,"maxAttempts":10,"delayMs":2000,"errorMessage":"…"}`
   与 `{"type":"auto_retry_end","success":true|false,"attempt":1,"finalError":"…"}`。
   前端 `useAgentSession.ts` 用这两帧 `setRetryInfo` / `setRetryInfo(null)`（输入框上方
   那条「重试中…」），所以**必须成对**：只发 start 会让提示永远挂着。
   harness 只在「排定重试」时发事件，收尾这一半由
   `agent_runtime::RetryNotice` 配对：重试后首个正常助手 `message_end` → `success:true`；
   整轮 `agent_end` 仍未恢复 → `success:false` + `finalError`。
   失败那次尝试自己的 `message_end` 不收尾（后面可能还跟着下一次重试）。
4. 启动失败：`{"type":"startup_error","errorMessage":"Failed to start agent: …"}` 后关闭

会话不存在：`404 Session not found`；请求已中断：`204`。

### Responses 推理摘要（运行时适配）

活会话中，推理模型的实际档位为 `minimal/low/medium/high/xhigh/max` 时，统一请求
`reasoning.summary = "auto"`；`off` 不请求，不改变既有 effort。覆盖自定义
`openai-responses` provider 与内置 Coding（`apisets`）的 Responses 模型。
其他协议不加 Responses 专用字段，自动起标题等 oneshot 仍按原来的 off 请求。

依据：rpi-ai 0.25.0 的请求构造只有 `reasoning.effort`，而 pi 的 Responses 实现
默认请求摘要。返回的摘要沿用现有 `thinking_start/delta/end` 帧，`/api` 形状不变；
这是模型公开返回的推理摘要，不保证每次工具调用前都有内容，也不能缩短模型实际推理。

rpi 没有请求体扩展钩子且不能修改依赖源码，因此 `reasoning_summary.rs` 沿用
qoder-cn 的同进程回环转发方式：`POST /reasoning-summary/{token}/v1/responses`。
它不是公开 `/api`；只接受 loopback 对端和存活请求的随机令牌，目标仅由进程内注册，
不接受客户端指定 URL。逐块传递上游响应，保留状态码与 Retry-After，取消或结束时
撤销令牌并终止转发；不改配置中的模型地址、持久化消息或历史会话。

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
- 同名技能：**项目与用户之间合并**（同名也各留一条，项目在前）。pi 是全局按 name 去重，
  项目版本会把用户版本顶掉 —— 这里刻意不同。同一个文件被扫到两次才按 `filePath` 去重。
- `sourceInfo` 决定前端的组：`user` → Global，`project` → Project；两者都没有时落到 Other。
- `SKILL.md` **缺 `description`** 会被直接丢弃且**不产生诊断**（pi 与 toiler 一致），
  “技能写好了却不显示”先查 frontmatter（如新建的 `k8s-deploy` 曾因此消失）。
- 这份列表与系统提示词的 `<skills>` 段**共享同一份扫描结果**
  （`crate::skills::load`）。只改一处扫描规则而漏了另一处，就会出现「面板里
  启用了、模型却看不到」这种静默失效 —— 以前正是如此（提示词里根本没有技能段）。

### 技能目录（决定「不同项目不同技能」）

**只有两个来源**：一个项目目录 + 一个用户目录。

| 来源 | 扫描范围 | `scope` |
|---|---|---|
| 项目 | **`$project/.agents/skills` 一层**。`$project` = 从 `cwd` 向上最近的 git 仓库根；不在 git 里就是 `cwd` 本身（跳过 `~/.agents/skills`） | `project` |
| 用户 | `~/.agents/skills` | `user` |

**不扫** `~/.pi/agent/skills`（pi 的 agent 目录）与 `~/.agents/toiler/skills`：
技能目录只有一处约定，用户不必猜自己的技能为什么在一个地方生效、换个 agent 目录
就失效。从 pi 迁过来的用户想继续用原有技能，把目录搬（或链接）到
`~/.agents/skills` 即可——首次启动的配置播种只搬 `auth/models/settings/trust/
git-credentials` 五份文件，**不搬技能目录**。

两条前提，对应 pi 的 `collectAncestorAgentsSkillDirs` + `projectTrusted`：

1. **只认项目根**：约定位置就是 `$project/.agents/skills`。子目录里开会话
   （`$project/frontend`）仍读项目根那一份；子目录自己的 `.agents/skills` 不算。
   与 pi 的差异：pi 在找不到 git 根时会一路扫到文件系统根（`~/Development/.agents/skills`
   也会被当成项目技能），toiler 只认项目根，行为更好预测。
2. **项目必须已信任**：未信任时项目技能**完全不加载**，同时 `projectResourcesLoaded=false`
   （前端据此禁用「Project」作用域按钮）。否则一个刚 clone 的仓库可以靠 `.agents/skills` 里的
   提示词影响 agent。

目录不存在不产生诊断（pi 会为它报 `list_failed`，toiler 直接跳过——「没装过项目技能」是常态）。

skills 市场接口由 Toiler 扩展实现：搜索读取 skills.sh 的公开 `/api/search`（当前为网站搜索页实际使用的接口），返回前端兼容的 `{results:[{package,installs,url}]}`。上游成功响应的响应体若意外中断，搜索会自动重试一次，两次请求共用 20 秒总时限；超时返回 504。安装仅接受 `owner/repo@skill` 形式的 GitHub 技能，默认优先通过 `https://gh-proxy.com/` 下载 GitHub 归档，失败后再尝试 GitHub 原地址；可用 `TOILER_GITHUB_MIRROR` / `TOILER_GH_PROXY` 自定义，设为 `off` 禁用。不启动 Node。skills.sh 官方 `/api/v1` 需要 Vercel OIDC，因此不直接依赖该接口。

## GET /api/models

（见 `tools/pi-contract/raw/models.js`；字段名与 pi 的 `Mx()` 逐字对齐。）

```jsonc
{
  "models": { "<provider>:<id>": "显示名" },        // 字典，不是数组
  "modelList": [{ "provider": "p", "id": "m", "name": "M" }],
  "defaultModel": { "provider": "p", "modelId": "m" } | null,
  "defaultThinkingLevel": "high" | null,          // Toiler 扩展：最近显式选择（off…max）
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
>
> **前端会把只有一档的模型（`["off"]`）的推理入口直接不渲染。** 这正是上面
> 「不该在非推理模型上显示推理菜单」的落地方式：`availableThinkingLevels` 只有 1 项时
> 菜单没有可操作性。以前能选 `auto`（默认推理）时菜单至少有 2 项，删掉 `auto` 后
> 才会出现「只有一项」的形态；`isThinkingLevelAdjustable` 守着这条。

### 可见范围

按 `settings.json` 的 `enabledModels` 白名单过滤（§`settings::visible_models`）。
`enabledModels` 里写成 `provider/id:level` 的条目会额外进 `thinkingLevelPins`。
最近显式选择的推理级别优先于该固定级别；未选择时按固定级别、`medium`
（`settings::DEFAULT_THINKING_LEVEL`）依次回退。
`defaultThinkingLevel` 是为跨启动恢复偏好新增的可选字段，旧客户端可直接忽略；
它只会是 `off…max` 七档之一，遗留的 `auto` 值读取时被当成「没选过」。

### `defaultModel` 必须在 `modelList` 里

`defaultModel` 与 `POST /api/agent/new` **共用同一套解析**
（`AppState::default_model_ref()`），顺序与 pi 的 `selectInitialModelScope` 一致：

| 顺序 | 来源 |
|---|---|
| 1 | `settings.json` 的 `defaultProvider`/`defaultModel` |
| 2 | `ModelCatalog::default_entry()`（启动时由 `TOILER_MODEL` / 上次用过的模型定下）|
| 3 | 可见范围里第一个能解析的模型 |

**每一步都要校验 `enabledModels` 可见范围**。前端 `useAgentSession.ts:loadModels()`
拿 `defaultModel` 去 `modelList` 里 `find`，找不到就**静默**退回 `modelList[0]`：

```ts
const displayModel = match ?? nextModelList[0];
```

所以只要 `defaultModel` 落在 `modelList` 外，输入框就会显示成列表里排第一的模型，
而真正建会话用的是设置里那个 —— 选择器写着 A、回复来自 B，切换前后都不给提示。

历史故障：`/api/models` 早先用的是 `catalog.default_entry()`（`models.json` 里第一个
可用项），它被 `enabledModels` 排除在外（如 `OpenAI/gpt-5.4`，而可见范围里只有
`OpenAI/gpt-5.6-sol` 等），于是界面显示 `GPT 5.6 Sol`、实际会话走
`settings.json` 的 `apisets/deepseek-v4.1-flash`。

回归用例：`tests/regression.rs::default_model_is_inside_the_visible_model_list`、
`tests/regression.rs::default_model_falls_back_within_the_visible_scope`。

### 历史故障：`models` 曾经是数组

阶段 2 实现返回的是 `{models: [{…}], default: {…}, source}`，缺少
`modelList` / `defaultModel` / `thinkingLevels*`。前端把 `models` 当
`Record<string,string>` 用，拿到数组后 `modelId` 退化成数组下标 `"0"`、
`name` 变成整个对象，`Intl.Collator.compare()` 收到对象抛 TypeError，React
树被卸载 → 点「选择模型」时整页变成 Next 的 *This page couldn't load*。

回归用例：`tests/regression.rs::models_dict_and_list_share_the_same_keys`、
`tests/regression.rs::thinking_levels_match_pi_semantics`。

## POST /api/models-config/refresh（Toiler 扩展）

依据：API Key 供应商详情与已实现订阅供应商详情原先只有本地目录重读，没有远端模型刷新入口；
新增与前端刷新图标配套的写侧接口，不改变原有 `/discover`（仅发现、不落盘）的含义。

请求：`{providerName:"openai"}`，只传内置供应商 id，凭据不返回浏览器。
成功返回 `{models:[{id,name?,contextWindow?,maxTokens?,input?}],endpoint,refreshed:true}`。
Qoder CN 使用相同响应外形，但模型定义来自官方 `model/list`，并按当前 OAuth 账号缓存。

- 用已保存的凭据请求远端 `/models`；普通供应商结果写入同目录的
  `models-store.json`（`etag=builtin-models-api-v1`），并立即注入运行时目录；
  Coding 继续走自己的多协议映射和 `apisets-models-api-v2` 缓存。
- 不重写 `models.json` 或 `settings.json`；用户模型与远端目录取并集，同 id
  优先采用用户元数据。远端旧模型会被新快照替换；失败不会清空旧缓存。
- `qoder-cn` 的缓存带 `accountUid`：换个账号登录后旧账号的模型不会串用。
- `enabledModels` 白名单原样保留：新模型不一定自动显示在对话框，要在设置中勾选。
  普通供应商重启复用缓存，只在用户手动刷新时联网；Coding 保留原有启动预热。
- 400：缺供应商/Key/Base URL、不适用的供应商；409：已有刷新在进行；
  501：协议尚不支持或该订阅尚未实现目录刷新；502：上游拒绝、非 JSON 或空模型列表；
  504：超时。接口沿用 Host/Origin 校验，订阅令牌不会降级用作 API Key。
- 订阅分流：`qoder-cn` 要求 OAuth 已登录，用 COSY 签名的官方 `GET /algo/api/v2/model/list?Encode=1`
  （20 秒超时、不跟重定向；401/403 先续期一次）；目录写入同一个 `models-store.json`
  （`etag=qoder-cn-models-api-v1`、`accountUid`），账号变化后自动回落静态快照；
  刷新成功会重建运行时目录，新模型在对话里可直接选用。其它 OAuth 订阅返回 501。
  Qoder CN 若存在同名用户自定义 provider 配置，改用自定义 Provider 的导入模型入口。

## GET /api/appearance/season（Toiler 扩展）

新建对话页面的 `SeasonalIllustration` 使用，和运行中的应用图标共用
`toiler-server::seasons` 的离线四立天文历。按立春、立夏、立秋、立冬的实际交节
时刻切换，时间戳均为 Unix 毫秒，不按本机时区或固定月份划分。

```jsonc
{
  "season": "winter", // spring | summer | autumn | winter
  "serverTime": 1795651200000,
  "nextChangeAt": 1800000000000 // 示例值，实际为下一次四立交节时刻
}
```

响应带 `Cache-Control: no-store`。前端按服务器时间差调度，临近交节时定时到边界，
平时最多每分钟重新判断，并在恢复前台后刷新。图片路径是前端内置的四张 WebP，
不由接口下发任意 URL，不访问第三方服务，不读取或写入用户 Store。计算失败返回
`500 {"error":"读取季节失败"}`；前端保留现有图片或回退为原品牌标记。

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

## 补齐的前端调用链

依据 `tools/pi-contract/raw/` 中的 `app-update.js`、`file-index.js`、
`checkpoints.js`、`skills_check.js`、`skills_update.js`、`skills_remove.js`、
`files_reveal.js`，补记原路由表遗漏的接口：

- `GET /api/app-update?refresh=1`：`{currentVersion,latestVersion,updateAvailable,releaseUrl}`。
  发布源改为 `coder-nil/toiler`，5 秒超时、成功结果缓存 12 小时；普通查询在上游失败时
  可回旧缓存，强制刷新失败回 `502 {error}`，不会跳转到 pi-desktop 的发布页。
- `GET /api/file-index?cwd=…&q=…`：无 q 返回 `{files:string[],truncated:boolean}`，
  首批最多 5000 个文件；带 q 返回 `{matches:[{path,isDir}]}`，最多 20 项。
  cwd 必须是允许根中的绝对目录，并通过 realpath 判定；Git 仓库按 `.gitignore` 枚举，
  否则有限深度扫描（不跟随目录符号链接）。与 pi 的差异：当前不缓存索引，
  权限每次重新检查；扫描有时间、条目数量和深度上限。
- `GET/POST /api/checkpoints`：`501 {error:"…中文说明…"}`，尚无工作区文件快照或回滚。
  **不伪造 checkpoint id**；前端发送消息仍保留尽力而为的调用，不影响正常对话。
- `POST /api/skills/check`：`{cwd, package?, scope?}` →
  `{updates:[{package,scope,state,currentVersion,latestVersion?,message?}]}`。不指定包或作用域时，
  检查可见的 Toiler 安装记录（未信任的项目跳过）；同一仓库在一次检查中只下载一份归档。
  比较技能目录内容哈希，而不是仓库提交号；单个上游失败时，该项返回 `state:"error"`，
  不影响其他检查结果。
- `POST /api/skills/update`：`{cwd,package,scope}` → `{success:true,package,scope,skill:{install}}`。
  下载并完整写好新版才替换对应目录；失败不覆盖旧版，版本相同不重写。升级替换整个包目录，
  不合并本地修改。新安装会记录归档中的技能路径，避免上游移除目标后误装同仓库的其他技能。
- `POST /api/skills/remove`：`{cwd,package,scope}` → `{success:true}`，不联网，只删除对应的 Toiler
  安装目录。升级和卸载都校验 `.toiler-skill.json` 中的来源、包名和作用域；手动技能、路径链接
  和未信任的项目不允许修改。旧安装记录不必迁移即可检查、升级和卸载。
  Toiler 的这些操作均不执行上游 `npx skills`。
- `POST /api/files/reveal`：请求 `{filePath, sessionId?}`，成功 `{ok:true}`。
  判定顺序与 pi 一致：绝对路径 → 允许根词法包含或会话引用过该路径 → 目标存在
  → 非引用授权时再过 realpath 包含 → 交给系统文件管理器
  （macOS `open -R` / 目录 `open`，Windows `explorer /select,`，Linux `xdg-open` 父目录），
  10 秒超时。**与 pi 的差异**：手机与公网隧道一律 403 —— 两者都经 `lan_proxy`，
  该代理会追加 `x-toiler-remote-client` 标记头（客户端自带的同名头会被丢掉），
  否则远端能随意弹本机文件管理器窗口。

以上接口均做 Host/Origin 校验。501 代表明确未实现，不代表功能已经补齐。
手机页 `/m` 则是实际静态页面：支持会话快照、流式对话、运行中引导、停止、新建任务、
历史窗口和扩展问答；不提供任意系统操作。`agent_end` 触发最终快照刷新。

## 实现进度（`crates/toiler-server`）

| route | 状态 | 备注 |
|---|---|---|
| `GET /api/app-update` | ✅ | 本项目 GitHub Releases 检查，12 小时缓存；上游失败 502 |
| `GET /api/file-index` | ✅ | `@文件` 补全：files/truncated 或 matches；允许根 + realpath 校验 |
| `GET/POST /api/checkpoints` | ❌ | 501 + 中文原因，不执行文件快照或回滚 |
| `POST /api/files/reveal` | ✅ | 打开系统文件管理器（`open -R` / `explorer /select,` / `xdg-open`），10 秒超时；允许根 + realpath 校验；手机与隧道 403（`x-toiler-remote-client` 标记头） |
| `GET /api/sessions` | ✅ | 字段全对齐（含 `sessionsUpdatedAt`、30s 缓存、`?force=1`） |
| `GET /api/sessions/{id}` | ✅ | `info`/`tree`/`context`/`paging`/`totalActiveMs` 全对齐 |
| `PATCH /api/sessions/{id}` | ✅ | 追加 `session_info` 条目（pi 的做法） |
| `DELETE /api/sessions/{id}` | ✅ | 关活 agent + 删文件 |
| `GET /api/sessions/{id}/state` | ✅ | 与 `GET /api/agent/{id}` 同实现 |
| `GET /api/sessions/search` | ✅ | 元数据 + 文本块 + compaction.summary，58 字符 snippet |
| `POST /api/agent/{id}` | ✅ | `prompt`/`steer`/`follow_up`/`clear_queue`/`abort`/`get_state`/`set_model`/`navigate_tree`/`set_steering_mode`/`set_follow_up_mode`（后两个只落盘，`applied:false`，新会话生效；见 `GET/PUT /api/queue-modes`） |
| `GET /api/agent/{id}` | ✅ | `{running}` / `{running, state}` |
| `GET /api/agent/{id}/events` | ✅ | SSE，裁剪规则与 pi 逐条对齐 |
| `POST /api/agent/new` | ✅ | 建 v3 会话 + 登记项目 + 执行 body 里的命令；`{success,sessionId,data,model,thinkingLevel}`。body 的 `toolNames`（前端工具预设，默认 `read/bash/edit/write`，桌面工具始终启用）、`uiLocale` 与 `chatMode`（普通对话模式：第一轮就不带系统提示词）也在建会话时应用 |
| `POST /api/agent/draft/system-prompt` | ✅ | Toiler 扩展：只读预览，接收 `{cwd,toolNames?,uiLocale?,chatMode?}`，返回 `{systemPrompt}`。仅接受允许根内的绝对目录；与运行时共用工具筛选和提示词组装，不建会话、不登记项目、不保存偏好。 |
| `GET /api/agent/running` | ✅ | `{runningSessionIds, pendingUiSessionIds}`（pi 的 `56429.am()`） |
| `GET /api/files/{*path}` | ✅ | 六种 `type` 全实现（`list`/`read`/`download`/`meta`/`preview`/`watch`），`Range` 206/416，SSE 命名事件 `connected`/`change` |
| `PUT /api/files/{*path}` | ✅ | `{content, allowEmpty}`；空覆盖 409 `empty_overwrite_blocked`；256KB 上限 |
| `PATCH /api/files/{*path}` | ✅ | 重命名；纯文件名校验；撞名 409 `already_exists` |
| `POST /api/files/{*path}` | ✅ | `?type=upload-check` / `?type=upload&conflict=error\|overwrite\|skip`；25MB 单文件 / 100MB 总量；部分成功 207 |
| `GET /api/models` | ✅ | `{models, modelList, defaultModel, thinkingLevels, thinkingLevelMaps, thinkingLevelPins, source}` 全对齐；`models` 是 `provider:id` 字典；`defaultModel` 与建会话共用 `AppState::default_model_ref()`，**必定是 `modelList` 里的一项**（见下方「`defaultModel` 必须在 `modelList` 里」） |
| `GET /api/skills` | ⚠️ | 项目技能：`$project/.agents/skills`（项目根 = 最近 git 根，需已信任）；用户技能：`~/.agents/skills`；两组合并返回（同名不互相顶掉）。不扫 `~/.pi/agent/skills` / `~/.agents/toiler/skills`。pi 的 `skillPaths`/package 技能没有（无 package 系统），`usageCount` 与缓存也不提供（`cacheHit` 恒 false） |
| `POST /api/skills/search` | ✅ | `{query}` → `{results:[{package,installs,url}]}`；数据来自 skills.sh 公开 `/api/search`，上游失败返回 502/504 |
| `POST /api/skills/install` | ✅ | `{package:"owner/repo@skill",scope:"global"|"project",cwd}`；默认优先经 `gh-proxy.com` 下载 GitHub 归档，失败后回退 GitHub 原地址；校验路径与大小后安装到 `.agents/skills`，已有目录返回 409 |
| `POST /api/skills/check` | ✅ | 单个或批量比较技能内容哈希，返回 `up-to-date` / `update-available` / `error` |
| `POST /api/skills/update` | ✅ | 只升级指定作用域的 Toiler 安装，暂存新目录后替换，失败保留旧版 |
| `POST /api/skills/remove` | ✅ | 离线卸载指定作用域的 Toiler 安装，不删除手动技能或链接目标 |
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
| `GET /api/sessions/{id}/export` | ⚠️ | 单文件 HTML（header/文件名规则同 pi）；内容是自己渲染的转写稿：左侧大纲 + 系统提示词分节 + 按轮次分组 + markdown 渲染 + 工具调用与结果按 id 合并成一个折叠块 + 思考折叠；`?theme=light\|dark` 控制深浅色，见差异 12 |
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
| `PUT /api/models-config` | ✅ | 保留 `$schema`、清洗无 id 的模型、`cost` 缺项补 0。**写盘后当场重建运行时目录**（见差异 25），新增 provider / 模型立刻进入 `/api/models` 的 `modelList` |
| `DELETE /api/models-config` | ✅ | `{kind:"provider"\|"model", provider, modelId?}`，顺手清 `enabledModels`；同样会重建目录，删掉的项不再出现在 `modelList` |
| `GET /api/models-config/enabled?provider=` | ✅ | `{provider,models[],enabledModels,unscoped,warnings:string[],refreshing}`；`warnings` 是**字符串数组**（pi 取 `resolveModelScopeWithDiagnostics` 的 `diagnostics.map(d => d.message)`），前端逐条当文本渲染，给对象会整页崩。`apisets`（Coding）的清单来自远端目录缓存，空列表时会顺手发起一次后台刷新并回 `refreshing:true`（见差异 21） |
| `PUT /api/models-config/enabled` | ✅ | `{provider,modelIds}` → 折叠成 `provider/*` 或逐条写 `settings.json` |
| `POST /api/models-config/discover` | ✅ | 打上游 `/models` 并按 pi 的字段兼容表解析 → `{models,endpoint}` |
| `POST /api/models-config/refresh` | ✅ | Toiler 扩展：刷新内置 API Key 或已实现订阅供应商的远端目录，缓存并注入运行时，保留用户配置与白名单 |
| `POST /api/models-config/test` | ✅ | 真跑一次补全 → `{ok,latencyMs,status,responseText}` |
| `GET /api/models-config/catalog` | ⚠️ | models.dev 目录已砍，返回空表 + `note` |
| `GET /api/git?cwd=` | ✅ | 分支/上游/增删/未跟踪/凭据类型/进行中的操作；`tags` 是 toiler 追加的字段（见差异 22） |
| `POST /api/git` | ✅ | pi 的 20 个动作（含 `merge_branch`、`*_remote_branch`、`*_branch`）+ toiler 追加的 `create_tag` / `delete_tag` / `push_tag` / `push_tags`（见差异 22、23），返回操作后的状态 |
| `GET /api/git/status?cwd=` | ✅ | `{isGitRepository,repositoryRoot,files[],additions,deletions}` |
| `GET /api/git/diff?cwd=&path=` | ✅ | `{supported,status?,patch?}`；未跟踪文件会合成补丁 |
| `POST /api/git/commit-message` | ✅ | NDJSON：`update` → `done`（失败 `error`）；用 `settings.json` 的默认模型，按请求的 `locale`（`zh-CN` / `en`，缺省 `en`）明确要求标题摘要和正文的输出语言 |
| `GET/POST/DELETE /api/worktrees` | ✅ | 列出 / 新建（`action:"checkout"` 走切分支）/ 删除（脏工作区 409 + `dirty:true`）；GET 额外返回当前工作树分支相对本地上游引用的 `branchSync: {ahead,behind}`，没有上游或不是工作树根目录时为 `null`。`GET /api/git` 和 `GET /api/worktrees` 尝试先获取当前分支的上游远程引用，失败时用本地已有引用；`GET /api/git/status` 不联网 |
| `GET/PUT/PATCH/DELETE /api/projects` | ✅ | 用 toiler 自己的 `<appDir>/work.sqlite`（`~/.agents/toiler/work.sqlite`）；接口形状与 pi 一致，但**不再共库** |
| `GET/POST /api/project-trust` | ✅ | `{requiresTrust,trusted}`；`trust.json` 与 pi 同格式 |
| `GET /api/plugins?cwd=` | ⚠️ | 只读：列 `settings.json` 的 `packages` + 安装状态 + 包内资源；安装根只认 toiler 自己的 agent 目录（见差异 19） |
| `POST /api/plugins` | ⚠️ | `install`/`remove`/`update`/`disable`/`enable` 全支持：npm 操作先检测本机可运行的 Node.js 与 npm（`--legacy-peer-deps`，安装根 `<toilerHome>/plugins/npm`），启停改 `settings.json`；**git: 来源不支持**；无可用 Node.js / npm → 500 + 中文提示。注意：装上的 TS 扩展 rpi 不会加载，功能不生效 |
| `GET /api/plugins/catalog?q=&from=&size=` | ⚠️ | 从 [Pi 官网目录](https://pi.dev/packages) 提取社区包；空 `q` 默认按月下载量排序，非空交给官网搜索。`from` 为偏移量（默认 0），`size` 默认 40、范围 1–50；返回 `{plugins,from,size,hasMore}`，上游失败返回 502，不伪装成无结果（差异 19） |
| `POST /api/plugins/catalog/translate` | ⚠️ | 保留 `{translations:{}}` 兼容响应；目录展示作者原文，不请求翻译服务 |
| `GET /api/mcp?cwd=&scope=&includeContent=` | ⚠️ | 读配置文件 → `{path,servers}`；`includeContent=1` 再带 `content` 原文（文件不存在时是 `{\n  "mcpServers": {}\n}\n`）。global 落在 `<configs>/mcp.json`（见差异 20） |
| `PUT /api/mcp` | ⚠️ | `{cwd,scope,content}` → 校验 `mcpServers` 是对象后原子写回 → `{path,servers}`；项目作用域未信任 403 |
| `POST /api/mcp` | ⚠️ | `{cwd,scope,presetId}` → 把内置 preset 的 `entry` 写进 `mcpServers` → `{path,serverName,added,servers}`；未知 preset 404 |
| `POST /api/mcp/test` | ⚠️ | 探活：`url` 发一次 `initialize`（10s）→ `passed`/`auth-required`/`failed`/`unknown`；`command` 跑 `--version` → `configured`/`failed`。**不建真实 MCP 会话** |
| `GET/PUT /api/tools/settings` | ⚠️ | `{isWindows:false,powerShellEnabled:false}`；PUT 501（shell 工具是 bash，没有 PowerShell 通道） |
| `GET/PUT /api/queue-modes` | ⚠️ | `{steeringMode, followUpMode}`（`all` / `one-at-a-time`，缺省 `one-at-a-time`），读写 `settings.json` 同名键；非法值 / 空 body 400。差异：pi 的 `AgentSession` 能改内存里的队列模式，rpi 的在 `AgentHarness::create` 时就定下来（`AgentHarnessOptions::steering_mode` / `follow_up_mode`），没有运行期 setter，所以 `PUT` 与 `set_steering_mode` / `set_follow_up_mode` 命令只对**新会话**生效；`GET` 必须实现，否则常规页在「完成提示音」下面常驻一行 `Not found` |

### `/api/files` 的实现细节（阶段 5）

- **路径形状**：POSIX 接受 `/api/files//Users/me/a.ts` 与 `/api/files/Users/me/a.ts`；
  Windows 接受盘符路径（如 `/api/files/C%3A/project/a.ts`）、UNC 和 `\\?\` 扩展路径。
  前端对 UNC 或扩展路径的双斜杠用 `%2F%2F` 保留，后端也兼容旧前端省略斜杠的 `?/…`。
  URL 路径只解码一次；`/api/files` 和尾部仅有斜杠时使用默认 cwd。非空路径不猜测相对 cwd。
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
   **`deferMedia`** ✅ 已实现，但**只作用于 `toolResult`**：`content` 里的图片块换成一行
   `[image: <mime>, ~<KB> KB]` 文本块（嵌套 `source` 与扁平 `data`/`mimeType` 两种形状都认）。
   前端首屏一直带 `deferMedia=1`，之前是空实现，带不带返回的字节完全一样——
   一条有图的会话首帧能到 1.38MB，实现后降到 214KB（实测同一条会话）。

   **为什么不处理 `user` 消息里的图**：早期实现不分 `role` 一律替换，于是重开有图的
   历史会话时，用户提问里亲手附的图永久变成一行 `[image: …]` 文字。thinking 有
   `entries/{id}/thinking` 可以点开回捞，图片没有等价通道，占位后就真取不回来了。
   工具抓的图（一次 screenshot 可能几十张）是历史噪声，占位合理；用户附的图受前端
   `MAX_ATTACHED_IMAGES`（10 张）约束，是「我给他看的这张」这种必须有语义的东西。

   注意：占位只发生在 `GET /api/sessions/{id}` 与 `/context` 的 `context.messages` 上，
   `/api/sessions/{id}/entries/...` 取单条、导出、手机投影一律返回原字节。
   `/context` 必须认这两个参数：前端翻页（`loadEarlierMessages`）也带着它们，少了
   就会「首屏的图是占位、翻上去的图是完整 base64」，同一屏两种表现。
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
   - **内置模型目录并非完整静态快照**：`/api/models` 读取 `models.json` + 内置目录
     （`qoder-cn` 的快照、`apisets` 的远端缓存，见差异 21），普通 API Key 供应商
     可在详情页点「刷新模型列表」直接请求兼容的 `/models` 并缓存（见上述 Toiler 扩展）。
     不提供模型发现接口或协议不受支持的供应商仍需手动配置，未引入 models.dev。
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
   与 pi 的差异：仅在保存了 Qoder CN OAuth 凭据后加载内置模型目录，退出后从可选模型中移除；
   `models.json` 中同名的自定义配置仍优先，未登录时也能在订阅区找到 Qoder CN 登录入口。
   已登录时没有当前账号的远端目录则使用静态快照；设置页刷新会用 OAuth 凭据和 COSY 签名请求
   官方 `model/list`，目录缓存绑定当前账号并同步到本机转发层。其它未实现 OAuth 目录链路的
   订阅会返回明确提示。
9. **`/api/auth/api-key/{provider}` 的 `displayName`** 取 `models.json` 的 `name`，缺省用 id
   （pi 取 models.dev 的展示名）。
10. **git 凭据**：与 pi 同格式 —— AES-256-GCM 加密存进 `git-credentials.json`
   （`<toilerHome>/configs/` 下），密钥是 macOS Keychain 里那条
   `pi-desktop-git-credentials` / `encryption-key`。两边**共用同一个钥匙串条目**，
   所以 pi 存过的密文 toiler 能直接解开，首次启动也会把它那份文件复制过来
   （见 `paths::SEEDED_CONFIG_FILES`）；反之亦然。
   差异：
   - 文件位置的 `agentDir` 换成 toiler 自己的 `configs/`（数据目录改名的一部分）；
   - 非 macOS 平台没有等价的系统密钥库，所以**不启用**加密存储（明确回「不可用」，
     而不是退回同一目录下的一把明文密钥）。这时仍可走 git 自己的 credential
     helper（`osxkeychain` / `manager` / `store`），只是 SSH 私钥口令无法记住。
   - `/api/git` 的返回体多一个 `canRememberCredential: boolean`（pi 没有），
     前端据此把「记住凭据」勾选框置灰并说明原因。

   `hasSavedCredential` / `savedCredentialUsername` 现在是**真实状态**（早先是写死的
   `false` / `null`）。这不是纯新增字段 —— 它修的是一个真问题：前端靠这两个字段
   决定「口令框该不该被清空」，写死 `false` 就等于每次推/拉之后把刚记住的口令抹掉，
   于是勾了「在此设备上加密记住凭据」也永远要重输。同时 `fetch` / `pull` / `push` /
   `push_tag` / `push_tags` 在请求体不带 `credential` 时会自动取已存的那份，
   不再依赖 credential helper（那条路对 SSH 口令无效）。
   credential helper（`git credential approve`）**仍然写**，但只对 HTTPS：
   它能让用户在终端里自己跑的 `git push` 也受益。
11. **`/api/git/commit-message` 的流**：pi 每收到一段就发一行 `update`；toiler 攒完再一次
    `update` + `done`（字段相同，少了中间增量）。
12. **`/api/sessions/{id}/export` 的 HTML**：响应头与文件名规则与 pi 一致，但内容是自己渲染的
    转写稿（pi 用 `pi` CLI 生成带树状导航的 HTML）。`?inline=1` / `?desktop=1` / `returnTo`
    都支持。另外 toiler 多一个 `?lang=zh-CN|en`（影响导出页自己的文案与 `<html lang>`）。
    页面结构与 pi 不同，按「可扫读」重排：
    - **按用户提问切轮次**：每条提问开一个 `<section class="turn" id="turn-N">`，
      它后面的助手正文与该轮工具调用都归进这一轮。第一条提问之前的内容归 `turn-0`
      （没有就不出现）。真实会话里工具输出占 76%，平铺会把主线淹掉。
    - **工具调用与它的结果合成一个折叠条**（`<details class="tool">`，默认收起）：
      v3 里「调用」和「结果」是**两条独立消息**，只靠 `toolCall.id` ↔
      `toolResult.toolCallId` 关联，所以渲染前先按 id 配对（`collect_tool_results`），
      调用块里固定两段「参数」与「结果」，摘要行带真实工具名、首行参数、结果行数与
      失败标记；没配上结果时明说一句而不是只剩参数。配对键要同时认 `id`（v3 原文）
      与 `toolCallId`（`context_view` 归一化后），只认一个会整页变成「没有记录到结果」。
      思考单独一个折叠条（`class="thinking"`）。折叠正文超过 4000 字会截断并提示原文长度。
    - **正文走 markdown 渲染**（`pulldown-cmark`：表格 / 删除线 / 任务列表），
      不再是转义后的纯文本（那会让 `\n\n` 被 HTML 折成空格、列表与围栏全部失效）。
    - **左侧常驻大纲**（`.outline`，260px 粘滞在左边）：首项是「系统提示词」，
      后面每轮一项（提问首行 + 时间 + 轮次号）。窄屏（≤1000px）改成顶部横向
      滚动的胶囊列表，不再占侧边宽度。
    - **系统提示词面板**：v3 文件把它存在 `message.sections` 里（`content` 是空串）。
      同一分支上会有**多条**（切工具 / 换模型时重发），后面的是**增量**——实测
      一份会话后两条只带 `tools`+`rules`，`project_context`（8 KB）只在第一条里。
      所以按分支顺序**合并**、同名分节以后者为准，不能只取最后一条。分节按
      `preamble → tools → rules → project_instructions → project_context → skills
      → docs → cwd → env` 排列，每个分节一张可折叠卡片（首个展开），纯文本
      原样展示（不跑 markdown：这里要看的是实际发出去的字节）。
    - 每条消息带 `timestamp` 的 `HH:MM:SS`（UTC）。
    - 工具字段**两种形状都认**：`{toolName,input}`（`normalize_tool_calls` 归一化后的）
      与 `{name,arguments}`（v3 文件原始形状）。只读后者会全部渲染成「🔧 tool」+ `null`。

    `?inline=1` 或 `?desktop=1`（页面被展示而不是下载）时额外注入一个常驻的
    「返回对话」按钮 + 一个「`Esc` 返回对话」提示条：桌面壳里 `window.open` 不建子窗口，
    前端是把 WebView **整页导航**到本页的，页面上没有浏览器后退键，用户进得去出不来。
    有白名单内的 `returnTo` 就 `location.replace` 回跳，否则 `history.back()`，
    没有历史时 `window.close()`。

    Esc 也能返回，但这个手势没有任何可见线索——不提示就等于不存在，所以进页面时
    在**页面底部居中**浮一个 toast（可点 `×` 关掉、6 秒后自动收起，`sessionStorage`
    记住本次停留已提示过）。toast 的背景/文字走 `--toast-bg` / `--toast-ink` 等
    主题变量，浅色与深色各一套，不要写死颜色。

    `?overlayChrome=1`（桌面壳按 `window.__TOILER_OVERLAY_TITLEBAR__` 传）给 `<html>`
    挂 `overlay-chrome`：macOS 用的是覆盖式标题栏，红绿灯浮在网页内容上（距顶约 42px、
    左侧约 76px），左侧大纲不往下让位的话第一项「会话目录」正好被按钮盖住。
    下载形态（`attachment`）不注入任何界面元素。

    `?theme=light|dark` 是**应用里已解析的主题**，前端从 `useTheme().theme` 透传。
    导出页是独立 HTML（桌面壳里还是整页导航），拿不到应用的 React 状态；只靠
    `@media (prefers-color-scheme)` 的话「系统深色 + 应用里选浅色」会让导出页永远黑着。
    显式主题落成 `<html class="theme-light|theme-dark">` + `<meta name="color-scheme">`，
    CSS 里类选择器 `html.theme-*`（权重 0,1,1）高于 media 里的 `:root`（0,1,0），因此
    即使系统是深色也能回到浅色。不传（`auto`）或值认不出时不挂类，交由系统偏好决定。
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
    一条插件画出来——这就是「不显示已安装插件」的直接原因。添加页的目录另从
    `pi.dev/packages` 服务端渲染的列表提取包名、简介、作者和月下载量，原样保留排序；
    只返回结构化数据，不向前端转发官网 HTML 或脚本。目录收录不表示 Toiler 能加载 TS 扩展。
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

24. **传输层中断会被补上可重试标记（pi 不会）**：上游在流式响应中途把连接重置
    （`Connection reset by peer (os error 54)` 一类）时，`rpi-ai` 会把它包成
    `AiError::Sse`，provider 发一个 `stopReason: "error"` 的终态。这道错误在 rpi
    的两道重试闸门上都过不去：

    | 闸门 | 为何放行不了 |
    |---|---|
    | provider 层 `retry_provider_request` | harness 把 `max_retries` 传成 `None`，且它只重试 `AiError::Http`，`Sse` 直接透传 |
    | harness 层 `is_retryable_assistant_error` | 拿 `errorMessage` 做**子串**匹配，表里没有 `connection reset` 这一族措辞 |

    这是上游判定表的共同缺口、不是移植退化：拿同一条报错喂 pi 原版的
    `RETRYABLE_PROVIDER_ERROR_PATTERN`（`pi-ai/dist/utils/retry.js`）同样返回
    false（它的表里有 `connection.?lost`、`other side closed`，但没有
    `connection reset`），并且 rpi 0.22.1 的这份表逐字未变。

    toiler 的做法（`crates/toiler-server/src/retry_provider.rs`）：在 harness 的
    `models` 上包一层 [`Provider`] 装饰器，**透传 `id`/`models`**，只在终态
    `Error` 的 `errorMessage` 末尾追加一段能命中 rpi 子串表的标记
    （`[connection lost]`），之后交给既有 `RetryPolicy`（默认指数退避、最多
    10 次）。原文保留在最前面——它要展示给用户，也会落进 v3 文件给 pi 读。

    | 项 | pi | toiler |
    |---|---|---|
    | 判定方式 | 正则（`connection.?lost` 允许中间夹字符） | 子串（rpi 的实现，不能改） |
    | 覆盖范围 | 更宽（`terminated`、`unable to forward upstream` 等） | **只补传输层中断**（配额 / 参数 / 鉴权类明确不打，否则白等十轮退避） |
    | 命中后 | 报错原文 | 原文 + `[connection lost]` 后缀 |

    另外补了一个 rpi 的缺口：SSE 在 chunked 编码中途 EOF 时 reqwest 会「正常」
    结束流，provider 却没发终态，rpi 那边就是 `ended without a terminal event`——
    那是**不可重试**的 run 失败。装饰器会补一个 `Error` 终态，让它走上重试。

    注意这层只装在 `AgentHarnessOptions::models`（活会话）上：同目录的
    `oneshot`（自动起标题 / commit message / 模型连通性测试）是「不重试、不落
    会话」的一次性补全，那边不能被改写文案。

25. **`PUT`/`DELETE /api/models-config` 会当场重建运行时模型目录（pi 是每次请求重建）**：
    toiler 的 `ModelCatalog` 是**启动时一次性构建的快照**，而 `/api/models` 的
    `modelList` 直接读它。pi 那边 `/api/models` 里现场 `getAvailable()`（每次请求
    都重新解析 `models.json`），所以不存在这个问题。

    历史故障：设置页保存 `models.json` 后只写文件、不重建目录，用户新加的
    provider 与模型**一个都不会出现在对话框的模型切换器里**，只有重启进程才
    出现 —— 用户看到的就是「加了但没生效」。删除同理：已删的项留在 `modelList`
    里，选中就报 `Model not found`。

    修法：`AppState::reload_catalog_from_models_file()` 从落盘的 `models.json`
    重建目录，再用 `AppState::sync_live_catalogs()` 推给所有活会话。活会话那一步
    不能省：`LiveAgent` 各自持一份目录副本（它比一次请求活得久），只更新
    `AppState` 的话，界面上的 `modelList` 有新模型、但 `set_model` 走旧副本仍报
    `Model not found` —— 变成「选择器里有、一点就报错」。推的时候同步调
    `harness.set_models()`，否则新 provider 的模型能选中却发不出去
    （`No provider registered for model provider …`）。

    重建失败时保留旧目录并把错误透给前端：宁可显示旧清单，也不要清空成空列表。
    回归用例：`tests/model_refresh.rs::saving_models_config_exposes_new_providers_to_the_picker`、
    `tests/model_refresh.rs::deleting_models_config_removes_them_from_the_picker`。

    > 可见范围仍然是另一回事：`enabledModels` 白名单存在时，新 provider 的模型
    > 仍需要在设置页勾选才会出现在对话框（与 pi 的语义一致）。界面上把「刷新上游
    > 模型」入口做成勾选列表标题里的图标，发现的模型直接作为「待导入」行画进同一
    > 个列表 —— 勾一下即完成导入并设为可见（前端发两次串行 PUT：先 `models.json`
    > 再 `enabledModels`，因为后者会拒绝 `models.json` 里还不存在的模型）。

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
