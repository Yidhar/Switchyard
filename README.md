# Switchyard

> 用一个桌面 App 管理你本机的 AI coding CLI。

Switchyard 是一个本地 AI 编程工作台。它把你电脑里已经安装并登录好的 **Codex CLI**、**Claude Code**、**Gemini CLI**、**Antigravity CLI** 等工具放到同一个界面里，让你更方便地选择项目、切换 provider、管理会话、控制权限、发送附件、查看流式输出和文件变更。

Switchyard 不提供模型账号，也不托管你的代码。它负责管理本机任务；具体模型能力、账号、计费和网络访问仍由你选择的 provider 决定。

## 下载和安装

推荐直接使用桌面 App。

### 方式一：下载桌面 App

1. 打开 [Switchyard 下载页](https://github.com/Yidhar/Switchyard/releases/latest)。
2. 下载适合 Windows 的文件：
   - `Switchyard_..._x64-setup.exe`：推荐，适合大多数用户。
   - `switchyard-gui.exe`：便携版，下载后可直接运行。
3. 安装或启动 Switchyard。
4. 确认你想使用的 provider CLI 已经在本机安装并登录。

> Windows 首次运行未签名应用时可能出现 SmartScreen 提示。如果你信任当前下载来源，可以选择继续运行。

### 方式二：从源码启动

如果你想体验最新代码，或暂时无法使用桌面 App 文件，也可以从源码启动。

需要先安装：

- Git
- Rust
- Node.js / npm
- 至少一个已经安装并登录的 provider CLI，例如 `codex` 或 `claude`

然后在终端中运行：

```powershell
git clone https://github.com/Yidhar/Switchyard.git
cd Switchyard
.\start-gui.ps1
```

`start-gui.ps1` 会自动安装前端依赖并打开 Switchyard 桌面窗口。首次启动可能需要等待几分钟。

## 你可以用 Switchyard 做什么

- 在一个 App 里使用 Codex、Claude、Gemini 或其他本机 provider。
- 为每个项目或任务创建独立 session，避免上下文混在一起。
- 在发送任务前选择权限模式，例如只读、允许修改当前项目、或完全访问。
- 看到模型的实时正文、工具执行、权限请求、文件修改摘要和 diff。
- 直接粘贴截图，或把图片、日志、文本文件拖进输入框。
- 同时保留多个 session，让长任务在后台继续运行。

## 使用前准备 provider CLI

Switchyard 调用的是你本机已有的 provider CLI。你只需要安装自己要用的 provider，不需要全部安装。

| Provider | 默认命令 | 用途示例 |
| --- | --- | --- |
| Codex | `codex` | 代码阅读、修改、执行任务。 |
| Claude Code | `claude` | 审查、分析、长上下文整理。 |
| Gemini CLI | `gemini` | 作为另一个模型入口。 |
| Antigravity CLI | `agy` | 作为另一个模型入口。 |

安装后，可以在终端里检查命令是否可用：

```bash
codex --help
claude --help
gemini --help
agy --help
```

如果某个命令在终端里也不可用，请先安装对应 CLI，或把它加入 `PATH`。如果命令可用但无法完成任务，请先直接运行该 CLI，完成登录、授权、模型选择或首次初始化。

## 第一次使用

### 1. 选择 workspace

打开 App 后，先选择一个 workspace。通常就是你希望 AI 阅读或修改的项目目录。

### 2. 选择 provider

选择这次要使用的 provider，例如 Codex 或 Claude。不同 session 可以使用不同 provider。

### 3. 选择权限模式

发送任务前，先选择 sandbox 权限：

| 模式 | 适合场景 | 含义 |
| --- | --- | --- |
| `read-only` | 解释项目、代码审查、风险分析 | 只读查看，不修改文件。 |
| `workspace-write` | 日常代码修改 | 允许修改当前 workspace 内的文件，适合大多数改代码任务。 |
| `danger-full-access` | 你完全信任的任务 | 不限制文件访问。只建议临时用于可信任务。 |

不确定时，先选 `read-only`。需要让模型改代码时，再切到 `workspace-write`。

### 4. 发送任务

你可以像聊天一样描述目标，例如：

```text
阅读这个项目，告诉我主要功能和启动方式。
```

```text
修复登录页表单校验问题，并说明改了哪些文件。
```

```text
只做代码审查，不修改文件。重点检查权限、路径处理和并发问题。
```

### 5. 查看过程和结果

任务运行时，消息区会持续显示：

- 模型输出的正文。
- 正在运行的工具或命令。
- 需要你确认的权限请求。
- 文件变更数量和摘要。
- 可展开查看的 diff。

如果出现权限确认卡片，需要你明确选择 approve 或 deny。没有确认前，任务不应该静默继续或被当作默认拒绝。

## 图片和文件附件

Switchyard 支持常见的附件输入方式：

- 从剪贴板粘贴截图或图片。
- 拖拽图片、日志、文本文件到输入框。
- 在消息区查看已发送图片预览。
- 点击图片预览查看大图。

示例：

```text
参考这张截图，把当前页面的按钮间距调小一点。
```

```text
根据我拖进来的日志文件，找出启动失败的原因。
```

## 会话和历史

建议按任务拆分 session：

- 一个 bug 一个 session。
- 一个功能一个 session。
- 一个审查任务一个 session。
- 不同项目使用不同 workspace。

这样可以减少上下文混乱，也方便你在多个任务之间切换。长会话中，大型文件变更和大型 diff 会被折叠，避免页面一次性渲染太多内容。

## 本地数据和隐私

Switchyard 默认把会话、附件、文件变更记录和本地数据库保存在当前项目的 `.switchyard/` 目录下。

这些数据只在你的电脑上使用，可能包含：

- 对话历史。
- 截图和附件。
- 文件修改摘要。
- 本地任务状态。

不要把 `.switchyard/` 提交到公开仓库。如果你要分享项目，建议先确认 `.gitignore` 已经忽略该目录。

## 常见问题

### 打开 App 后找不到 provider

先在终端中确认 provider 命令可用：

```bash
codex --help
claude --help
gemini --help
agy --help
```

如果终端也找不到命令，请先安装对应 provider CLI，或把命令加入 `PATH`。

### Provider 命令存在，但任务无法运行

通常是 provider 自己还没有完成登录、模型选择、首次授权或初始化。请先直接运行对应 CLI，确认它可以独立完成一次简单任务。

### 我应该选哪个权限模式

- 只想让模型阅读和解释：选 `read-only`。
- 想让模型修改当前项目：选 `workspace-write`。
- 需要访问工作区外的文件：优先添加允许访问的路径。
- 只有完全信任任务和环境时才选 `danger-full-access`。

### 图片或附件无法读取

请确认：

- 文件仍然存在，没有被系统清理。
- 文件在当前 workspace 或允许访问的目录中。
- App 有权限读取该路径。
- 如果是临时截图，建议重新粘贴或拖拽一次。

### 任务看起来还在运行

长任务可能会持续执行命令、读取文件或等待 provider 输出。你可以查看消息区中的运行状态、工具状态和权限请求。如果出现权限确认卡片，需要先 approve 或 deny。

### 会话很多以后变慢怎么办

可以把不同任务拆成更小的 session，并关闭暂时不看的大型 diff。Switchyard 会尽量折叠大块内容，但长会话仍建议按任务分开保存。

## License

MIT
