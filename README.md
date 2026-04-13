# Switchyard

Switchyard 是一个官方 coding CLI 路由器与统一 TUI 外壳。

项目目标不是重新实现模型，也不是重做 Claude Code、Codex 或 Gemini CLI 的能力，而是提供一层稳定、可切换、可编排的本地界面，使用户可以：

- 使用自己的 TUI/CLI 界面与主要 provider 交互
- 将消息直接发送到当前选定的主要 provider
- 在同一界面中查看上下文、命令执行、文件变更和 diff
- 让当前主要 provider 编排其他 provider 作为 analyst、worker 或 reviewer
- 维护一份独立于厂商实现的 canonical session

## 当前范围

当前目录只落文档，不落实现。

首批文档覆盖：

- 产品愿景与非目标
- 架构分层与数据流
- canonical session 与 orchestration 协议
- provider 能力模型与适配约束
- Rust workspace 模块拆分
- 工程规则、任务流程与路线图

## 文档索引

- [愿景](./docs/VISION.md)
- [路线图](./docs/ROADMAP.md)
- [总体架构](./docs/architecture/ARCHITECTURE.md)
- [Canonical Session](./docs/architecture/CANONICAL_SESSION.md)
- [Provider 执行模型](./docs/architecture/PROVIDER_EXECUTION_MODEL.md)
- [事件通路](./docs/architecture/EVENT_PIPELINE.md)
- [Provider 能力矩阵](./docs/architecture/PROVIDER_CAPABILITIES.md)
- [编排协议](./docs/architecture/ORCHESTRATION_PROTOCOL.md)
- [模块拆分](./docs/modules/MODULE_BREAKDOWN.md)
- [Provider 合同](./docs/modules/PROVIDER_CONTRACT.md)
- [仓库布局](./docs/development/REPOSITORY_LAYOUT.md)
- [本地工具链](./docs/development/LOCAL_TOOLCHAIN.md)
- [工程规则](./docs/development/ENGINEERING_RULES.md)
- [测试策略](./docs/development/TEST_STRATEGY.md)
- [任务流程](./docs/development/TASK_WORKFLOW.md)
- [ADR-0001](./docs/adr/0001-rust-first-cli-router.md)
- [ADR-0002](./docs/adr/0002-store-context-and-routing-boundaries.md)

## 核心判断

Switchyard 采用 Rust-first 路线，原因如下：

- 目标体验更接近 Codex CLI，而 Codex 当前最佳实践是原生 Rust TUI
- 需要稳定处理流式事件、长上下文和 diff 视图
- 需要在不依赖厂商私有 session 的前提下维护本地 canonical session
- 需要统一接入多家官方 CLI/SDK，而不是被任意一家的终端交互绑死

## 当前结构决策

为了避免边界模糊，当前文档已经明确：

- `switchyard-session` 只负责会话领域模型，不负责持久化
- `switchyard-store` 负责 canonical session、event log、summary cache 和 native bindings 的持久化
- `switchyard-context` 负责 Context Composer 和上下文裁剪策略
- Router 不是独立 crate，而是 `switchyard-core` 内部的应用层职责
- `switchyard-config` 是叶子支持 crate，不允许反向依赖业务 crate
- Windows 本地开发基线使用 `stable-x86_64-pc-windows-msvc`
