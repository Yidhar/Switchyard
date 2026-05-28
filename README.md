# Switchyard

> A local command center for AI coding CLIs.

Switchyard 把你已经安装并登录好的 **Codex CLI**、**Claude Code**、**Gemini CLI** 和 **Antigravity CLI** 放到同一个本地工作台里。你可以用 CLI、TUI 或桌面 GUI 与不同 provider 对话、切换 sandbox 权限、查看流式输出与文件变更，并在需要时把子任务委托给其他 provider 在后台执行。

Switchyard 不托管模型，也不替代任何厂商账号、订阅、登录或安全策略。它是一层运行在你本机的路由、会话、工作区和协作界面。

## 你可以用它做什么

- **统一入口**：用同一个命令或界面调用 Codex、Claude、Gemini、Antigravity 等 coding CLI。
- **按任务切换 provider**：当前 turn 可以指定 provider，也可以在配置里设置默认 provider 和常用 peers。
- **保留本地会话**：会话、事件、文件变更摘要和 artifacts 默认保存在项目本地的 `.switchyard/` 下。
- **查看实时过程**：在 TUI/GUI 中查看模型正文、思考/执行状态、终端输出、工具活动、文件编辑和 diff。
- **处理长会话**：GUI 面向长历史和大 diff 做了虚拟化/折叠处理，减少大型会话中的卡顿。
- **发送附件**：GUI 输入区支持文本、复制粘贴、拖拽文件以及图片/截图附件。
- **多会话并行**：不同 session 可以在后端同时运行，便于把互不相关的上下文隔离开。
- **后台委托**：通过 HYARD host bridge 让一个 provider 把分析、审查或 worker 任务交给另一个 provider。
- **权限控制**：每次运行可选择 `read-only`、`workspace-write` 或 `danger-full-access` sandbox 模式。

## 当前状态

Switchyard 目前处于早期可用阶段，适合愿意从源码构建、并且已经熟悉至少一个 AI coding CLI 的用户试用。

- Provider CLI 需要你自行安装、登录并放入 `PATH`。
- 默认优先支持 Windows 本地使用；Rust workspace 本身尽量保持跨平台。
- 桌面 GUI 目前以源码/dev 方式启动，正式安装包会在发布流程稳定后提供。

## 支持的 provider

| Provider 名称 | 默认命令 | 说明 |
| --- | --- | --- |
| `codex` | `codex` | 默认 provider。使用本机 Codex CLI 的账号、配置和权限流程。 |
| `claude` | `claude` | 使用本机 Claude Code CLI。适合审查、分析、长文本整理等任务。 |
| `gemini` | `gemini` | 使用本机 Gemini CLI。 |
| `antigravity` | `agy` | 使用本机 Antigravity CLI。 |

运行前建议先确认对应命令可用：

```bash
codex --help
claude --help
gemini --help
agy --help
```

不是每个 provider 都必须安装。你可以只安装并使用其中一个。

## 安装与构建

### 依赖

- Git
- Rust 1.85 或更新版本
- 需要使用 GUI 时：Node.js / npm，以及 Tauri 运行所需的系统依赖
- 至少一个已经安装并登录的 provider CLI，例如 `codex` 或 `claude`

### 从源码构建

```bash
git clone https://github.com/Yidhar/Switchyard.git
cd Switchyard
cargo build --release
```

构建完成后，二进制位于：

```text
target/release/switchyard
```

Windows 下是：

```text
target\release\switchyard.exe
```

如果你还没有把它加入 `PATH`，可以直接从仓库目录运行：

```powershell
.\target\release\switchyard.exe check
```

仓库根目录也提供了一个 Windows 便捷脚本，它会优先寻找 debug/release 构建产物：

```powershell
.\switchyard.cmd check
```

下文为了简洁统一使用 `switchyard` 作为命令名；如果你没有安装到 `PATH`，请替换为实际路径或 `./switchyard.cmd`。

## 快速开始

### 1. 检查 provider 与配置

```bash
switchyard check
```

需要机器可读输出时：

```bash
switchyard check --json
```

### 2. 运行单次任务

使用默认 provider：

```bash
switchyard run --message "Summarize this repository"
```

指定 provider：

```bash
switchyard run --provider claude --message "Review the recent changes and list risks"
```

指定 sandbox 权限：

```bash
switchyard run \
  --provider codex \
  --sandbox read-only \
  --message "Inspect the project and explain what it does"
```

允许 `workspace-write` 模式访问额外目录：

```bash
switchyard run \
  --sandbox workspace-write \
  --allow-path ../shared \
  --message "Update the integration code that depends on ../shared"
```

### 3. 打开终端界面

```bash
switchyard tui
```

常用选项：

```bash
switchyard tui --provider codex
switchyard tui --resume-latest
switchyard tui --session <session-id-or-prefix>
```

### 4. 打开桌面 GUI

Windows 源码环境可以直接运行：

```powershell
.\start-gui.ps1
```

这个脚本会：

1. 检查 GUI 前端依赖；如果缺失则执行 `npm install`。
2. 启动 Vite dev server。
3. 启动 Tauri 桌面窗口。
4. GUI 关闭后清理后台 dev server。

GUI 适合需要可视化管理多会话、查看文件 diff、处理图片/附件、切换 sandbox 权限以及同时观察多个 session 运行状态的场景。

## 配置

Switchyard 会按以下顺序查找配置：

1. 从当前工作目录向上查找 `switchyard.toml`。
2. 如果项目内没有配置，则尝试读取 `~/.switchyard/switchyard.toml`。
3. 如果都不存在，则使用内置默认值。

最小配置可以只写默认 provider：

```toml
[core]
default_provider = "codex"
```

一个更完整的示例：

```toml
[core]
default_provider = "codex"
default_peers = ["claude", "gemini"]

[sandbox]
mode = "workspace-write"
allowed_paths = []

[providers.codex]
command = "codex"
backend = "codex"
timeout_secs = 900

[providers.claude]
command = "claude"
backend = "claude"
timeout_secs = 900

[providers.gemini]
command = "gemini"
backend = "gemini"
timeout_secs = 900

[providers.antigravity]
command = "agy"
backend = "antigravity"
timeout_secs = 900

[store]
backend = "sqlite"

[ui]
show_diff = true
show_artifacts = true
```

配置说明：

- `core.default_provider`：未显式指定 provider 时使用哪个 provider。
- `core.default_peers`：常用后台委托 provider 列表。
- `providers.<name>.command`：实际执行的 provider CLI 命令；也可以写绝对路径。
- `providers.<name>.backend`：适配器类型，可用值包括 `codex`、`claude`、`gemini`、`antigravity`。
- `providers.<name>.args`：附加传给 provider CLI 的参数。
- `providers.<name>.env`：为该 provider 注入的环境变量。
- `providers.<name>.timeout_secs`：单次调用超时时间，默认 900 秒。
- `store.backend`：本地会话存储后端；通常使用 `sqlite` 即可。

如果你的 CLI 命令名就是默认值，并且只需要默认行为，可以不写 `[providers.*]`；Switchyard 会注册内置 provider 适配器。

## Sandbox 权限模式

Switchyard 的 sandbox 是本地文件访问策略，用于告诉 provider 这一轮允许怎样访问工作区：

| 模式 | 适合场景 | 行为 |
| --- | --- | --- |
| `read-only` | 代码审查、解释项目、风险分析 | 尽量只读，不应修改文件。 |
| `workspace-write` | 常规开发任务 | 默认推荐模式。允许在当前工作区内写入，并可通过 `--allow-path` / `sandbox.allowed_paths` 添加额外路径。 |
| `danger-full-access` | 你完全信任当前任务和工作区 | 不做文件系统 sandbox 限制。只建议在可信环境中临时使用。 |

CLI 中可以每次运行覆盖配置：

```bash
switchyard run --sandbox read-only --message "Review only; do not edit files"
```

GUI 中可以在发送前切换权限模式。遇到 provider 发起的权限请求时，GUI 会显示待处理 approval card；在用户明确 approve / deny 前，不应静默默认拒绝。

## HYARD host bridge：后台委托与多 provider 协作

`switchyard host` 提供机器可读的 bridge，适合把 Switchyard 嵌入其他 agent、脚本或 host pack。

列出可用 provider：

```bash
switchyard host list
```

发起后台委托：

```bash
switchyard host delegate \
  --provider claude \
  --task "Review the authentication module and report risks" \
  --wait-sec 1
```

如果返回 `wait_timeout`，并不代表失败；任务仍可能在后台运行。使用返回的 `job_id` 查询：

```bash
switchyard host status --job-id <job_id>
switchyard host result --job-id <job_id>
switchyard host await --job-id <job_id> --timeout-sec 30
```

Bridge 输出为紧凑 JSON，便于其他工具读取 `status`、`job_id`、`message` 和 `next_actions`。

## 本地数据保存位置

默认情况下，项目级数据保存在当前项目的 `.switchyard/` 目录下，例如：

- session 与事件记录
- SQLite store
- artifacts / 图片 / 文件附件
- 后台 job 状态

这些数据用于恢复会话和展示历史。Switchyard 本身不会把它们上传到远端仓库；你也不应该把包含私人会话、密钥或截图的 `.switchyard/` 提交到 Git。

## 常见问题

### `switchyard check` 提示 provider 不可用

先确认对应 CLI 是否安装并登录：

```bash
codex --help
claude --help
gemini --help
agy --help
```

如果命令不在 `PATH`，可以在 `switchyard.toml` 中把 `providers.<name>.command` 设置为绝对路径。

### Provider 已安装，但仍然无法执行任务

通常需要先在 provider 自己的 CLI 中完成登录、模型选择或首次授权。Switchyard 复用本机 provider CLI，不会代替它完成账号登录。

### GUI 启动失败

确认已经安装 Node.js / npm，并先构建过 Rust 项目：

```powershell
cargo build
.\start-gui.ps1
```

如果前端依赖损坏，可以删除 `crates/switchyard-gui/frontend/node_modules` 后重新运行 `./start-gui.ps1`。

### 我应该选哪个 sandbox？

- 不确定或只想看分析：选 `read-only`。
- 正常让 agent 改代码：选 `workspace-write`。
- 明确需要访问工作区外的路径：优先使用 `workspace-write` + `--allow-path`。
- 只有在完全可信时才使用 `danger-full-access`。

## License

MIT
