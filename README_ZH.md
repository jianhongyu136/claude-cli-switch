<div align="center">

# ccs（Claude CLI Switch）

### 轻量命令行工具：使用指定 Provider 启动 Claude / Codex / Gemini / OpenCode CLI

[![Version](https://img.shields.io/github/v/release/jianhongyu136/claude-cli-switch?color=blue&label=version)](https://github.com/jianhongyu136/claude-cli-switch/releases)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)](https://github.com/jianhongyu136/claude-cli-switch/releases)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)](https://www.rust-lang.org/)

[English](README.md) | [中文](README_ZH.md)

</div>

> 本项目基于 [CC Switch](https://github.com/farion1231/cc-switch)（MIT 协议），提取并扩展了其 CLI 能力为独立工具。

## 什么是 ccs？

`ccs` 是一个独立的命令行工具，可以使用指定 provider 的环境启动 Claude / Codex / Gemini / OpenCode CLI——**不修改全局配置文件**。每次调用完全隔离，你可以同时在多个终端使用不同的 provider。

从 v3.16.1 开始，`ccs` 还可以在选中的 provider 协议与目标 CLI 协议不匹配时，自动为本次调用启动临时本地代理并完成协议转换。

## 为什么用 ccs？

- **不修改全局配置** — 每次调用使用临时 settings/config 文件 + 进程级环境变量；`~/.claude/settings.json` 和其他 CLI 全局配置保持不变
- **自动协议路由** — Claude / Codex / Gemini 遇到需要协议转换的 provider 时，会使用当前进程专属的临时本地代理
- **多 provider 并发** — 在一个终端用 Provider A 跑 Claude，另一个终端用 Provider B，互不干扰；临时代理端口由系统自动分配，避免冲突
- **零额外配置** — 与 CC Switch GUI 共享同一个 SQLite 数据库（`~/.cc-switch/cc-switch.db`）；GUI 中添加的 provider 立即对 `ccs` 可用
- **轻量** — 约 3 MB 独立二进制，不依赖 Tauri/WebView

## 安装

从 [Releases](https://github.com/jianhongyu136/claude-cli-switch/releases) 页面下载对应平台的二进制文件，放到 `PATH` 中即可。

| 平台 | 文件 |
|------|------|
| Windows | `ccs-v{version}-Windows-x86_64.exe` |
| macOS (Universal) | `ccs-v{version}-macOS-universal` |
| Linux x86_64 | `ccs-v{version}-Linux-x86_64` |
| Linux ARM64 | `ccs-v{version}-Linux-arm64` |

也可以从源码构建：

```bash
cd src-tauri
cargo build --release --bin ccs
# 二进制位于：target/release/ccs（Windows 为 ccs.exe）
```

## 用法

```bash
# 使用 provider-first 语法启动 Claude / Codex / Gemini / OpenCode
ccs <provider-名称或id> claude
ccs <provider-名称或id> codex
ccs <provider-名称或id> gemini
ccs <provider-名称或id> opencode

# 透传参数给目标 CLI
ccs my-provider claude -p "你好"
ccs my-provider codex exec "修复这个问题"
ccs my-provider gemini -p "你好"

# 禁用自动临时代理（ccs 参数必须放在 tool 名称之前）
ccs my-provider --no-proxy claude -p "你好"
ccs my-provider --no-proxy codex exec "修复这个问题"

# tool 名称之后的参数属于目标 CLI
ccs my-provider claude --no-proxy   # 将 --no-proxy 透传给 claude

# 查看所有 provider
ccs list              # 所有应用
ccs list claude       # 仅 claude

# 查看当前激活的 provider
ccs status
ccs status codex

# 版本和帮助
ccs --version
ccs --help
```

Provider 查找优先匹配 **id**，其次按**名称**匹配（不区分大小写）。如果名称有歧义（多个 provider 同名），请改用 provider id。

## 退出码

| 退出码 | 含义 |
|--------|------|
| 0      | 成功（CLI 工具正常退出） |
| 2      | 数据库或 settings 文件错误 |
| 3      | Provider 未找到 |
| 4      | Provider 名称有歧义 |
| 5      | Provider 没有环境变量配置 |
| 6      | 临时代理启动或子进程代理配置生成失败 |
| 127    | 无法启动目标 CLI（不在 PATH 中） |

## 工作原理

1. 从 `~/.cc-switch/cc-switch.db` 读取 provider 配置（与 CC Switch GUI 共享）
2. 判断选中的 provider 是否可被目标 CLI 直连使用
3. 如果可直连：提取 provider 环境变量并启动子进程
4. 如果需要协议转换：启动使用系统自动分配端口的临时本地代理，并只让当前子进程连接该代理
5. 对 Claude：写入临时 settings 文件并通过 `--settings` 传入
6. 对 Codex/Gemini：通过进程级环境变量注入，必要时生成临时代理配置
7. 启动目标 CLI，退出后关闭临时代理并清理临时文件

## 前置条件

- 已安装 [CC Switch](https://github.com/farion1231/cc-switch) GUI 并添加了至少一个 provider
- 目标 CLI 工具已安装并在 PATH 中（`claude`、`codex`、`gemini` 或 `opencode`）

## 路线图

- [x] `ccs <provider> claude` — 启动 Claude CLI
- [x] `ccs <provider> codex` — 启动 Codex CLI
- [x] `ccs <provider> gemini` — 启动 Gemini CLI
- [x] `ccs <provider> opencode` — 启动 OpenCode CLI
- [x] `ccs <provider> --no-proxy <tool>` — 强制直连模式
- [x] Claude / Codex / Gemini 协议转换临时自动代理
- [x] `ccs list [app]` — 列出所有 provider
- [x] `ccs status [app]` — 显示当前激活的 provider
- [ ] `ccs switch <app> <provider>` — 切换激活的 provider
- [ ] Shell 补全（bash/zsh/fish/powershell）
- [ ] MCP 服务器配置转发

## 开发

```bash
cd src-tauri

# 构建
cargo build --bin ccs

# 运行测试
cargo test --bin ccs parser_tests
cargo test --lib cli::tests
cargo test --lib cli::proxy::tests
cargo test --lib proxy::server::tests

# Release 构建
cargo build --release --bin ccs
```

## 致谢

本项目基于 [CC Switch](https://github.com/farion1231/cc-switch)（作者：[@farion1231](https://github.com/farion1231)），复用了其 provider 数据库和环境变量提取逻辑。感谢 CC Switch 项目为 AI CLI 工具生态提供的基础设施。

## License

MIT
