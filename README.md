# Switchyard

Switchyard 是一个官方 coding CLI 路由器与统一 TUI 外壳。

项目目标不是重新实现模型，也不是重做 Claude Code、Codex 或 Gemini CLI 的能力，而是提供一层稳定、可切换、可编排的本地界面，使用户可以：

- 使用自己的 TUI/CLI 界面与主要 provider 交互
- 将消息直接发送到当前选定的主要 provider
- 在同一界面中查看上下文、命令执行、文件变更和 diff
- 让当前主要 provider 编排其他 provider 作为 analyst、worker 或 reviewer
- 维护一份独立于厂商实现的 canonical session

## 当前范围

当前仓库包含 Switchyard 的 Rust workspace、CLI/TUI/GUI、provider 适配层、host pack 与测试代码。

本地开发文档、ADR、路线图和研究笔记保存在仓库根目录的 `docs/` 下，但这些内容只用于本地迭代，不随公开 Git 历史发布。公开 README 只保留项目定位、核心判断和可运行入口，避免把未整理的内部开发记录暴露到 GitHub。

## 主要入口

- `crates/switchyard-cli`：命令行入口与 HYARD host bridge。
- `crates/switchyard-tui`：终端 UI。
- `crates/switchyard-gui`：桌面 GUI 与前端。
- `crates/switchyard-core`：turn runner、router、runtime event pipeline 与 provider proxy。
- `crates/switchyard-provider-*`：各 provider 适配层。
- `tests/`：跨 crate 集成测试、runtime observable/completion 测试与 CLI 回归测试。

## 核心判断

Switchyard 采用 Rust-first 路线，原因如下：

- 目标体验更接近 Codex CLI，而 Codex 当前最佳实践是原生 Rust TUI
- 需要稳定处理流式事件、长上下文和 diff 视图
- 需要在不依赖厂商私有 session 的前提下维护本地 canonical session
- 需要统一接入多家官方 CLI/SDK，而不是被任意一家的终端交互绑死

## 当前结构决策

为了避免边界模糊，当前实现遵循：

- `switchyard-session` 只负责会话领域模型，不负责持久化
- `switchyard-store` 负责 canonical session、event log、summary cache 和 native bindings 的持久化
- `switchyard-context` 负责 Context Composer 和上下文裁剪策略
- Router 不是独立 crate，而是 `switchyard-core` 内部的应用层职责
- `switchyard-config` 是叶子支持 crate，不允许反向依赖业务 crate
- Windows 本地开发基线使用 `stable-x86_64-pc-windows-msvc`
