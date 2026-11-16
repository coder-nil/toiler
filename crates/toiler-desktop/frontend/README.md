# 前端代码边界

本目录仅保留 Toiler 的界面代码、浏览器工具函数、接口响应类型和构建/测试脚本。
Next 负责静态导出；运行时 `/api`、会话读写、模型调用、配置存储和手机访问由
Rust 服务提供，agent 执行使用 crates.io 的 `rpi`。

## 不再保留旧 Node 服务

- 不引入 `@earendil-works/pi-*` SDK。
- `app/`、`components/`、`hooks/`、`lib/` 不导入 Node 内置模块。
- 原来的会话管理器、供应商适配器、配置文件读写、TS 工具实现已经移除。
- `lib/api-types.ts`、`session-stats.ts`、`agent-event-wire.ts` 等定义本服务的
  响应类型，不从旧 SDK 反推类型；修改时应核对 Rust 接口与 `docs/contracts/`。
- `lib/desktop-access.ts`、`mobile-pair.ts`、`model-catalog.ts`、
  `model-discovery.ts`、`plugin-catalog.ts`、`session-search.ts` 仅保留界面消费的类型，
  不执行配置读写、网卡探测、会话搜索或供应商请求。
- 手机组件及其辅助模块仍保留；未接入当前页面不意味着可以删除正在开发的代码。

Node 仍是前端构建和测试工具，不是应用运行时服务。

## 验证

在本目录执行：

```bash
npm install
npm test
npm run typecheck
npm run build
```

`frontend-boundary.test.mjs` 防止浏览器源码重新引入旧 SDK 或 Node 模块；
`service-client.test.mjs` 覆盖路径比较、系统消息过滤和 Rust SSE 增量的消费。

Tailwind 会扫描前端源码。清理文件时不能为了缩减目录而删掉仍被引用的组件和样式，
也不要在本目录放备份源码或无关大目录。
