## ccs v3.16.0 Release Notes

### 新增：`ccs` 命令行工具（Claude CLI Switch）

本版本新增独立 CLI 二进制 `ccs`，可在终端直接使用指定 provider 启动 Claude/Codex/Gemini，无需修改全局配置文件，支持多 provider 并发运行。

### 功能

- `ccs claude <provider>` — 使用指定 provider 启动 Claude CLI（隔离 settings 文件）
- `ccs codex <provider>` — 使用指定 provider 启动 Codex CLI
- `ccs gemini <provider>` — 使用指定 provider 启动 Gemini CLI
- `ccs list [app]` — 列出所有可用 provider（`*` 标记当前激活）
- `ccs status [app]` — 显示各应用当前激活的 provider
- `ccs --version` / `ccs --help`

### 使用示例

```bash
# 用 "my-relay" provider 启动 claude
ccs claude my-relay

# 列出所有 claude provider
ccs list claude

# 查看当前各应用激活的 provider
ccs status

# 透传参数给 codex
ccs codex my-provider -- --help
```

### 特性

- 与 GUI 共享同一数据库，GUI 中添加的 provider 立即可用
- Claude 使用临时 settings 文件 + 环境变量双重隔离
- Codex/Gemini 通过进程级环境变量注入，互不干扰
- 轻量独立二进制（~3 MB），不依赖 Tauri/WebView

### 下载

| 平台 | 文件 |
|------|------|
| Windows | `ccs-v3.16.0-Windows-x86_64.exe` |
| macOS (Universal) | `ccs-v3.16.0-macOS-universal` |
| Linux x86_64 | `ccs-v3.16.0-Linux-x86_64` |
| Linux ARM64 | `ccs-v3.16.0-Linux-arm64` |

下载后放到 `PATH` 中即可使用。

### GUI 安装包

- **macOS**: `CC-Switch-v3.16.0-macOS.dmg`（推荐）或 `.zip`
- **Windows**: `CC-Switch-v3.16.0-Windows.msi`（安装版）或 `-Portable.zip`（绿色版）
- **Linux x86_64**: `.AppImage` / `.deb` / `.rpm`
- **Linux ARM64**: `.AppImage` / `.deb` / `.rpm`

> macOS 版本已通过 Apple 代码签名和公证，可直接安装使用。
> `.tar.gz` 为 Tauri updater 自动更新专用，无需手动下载。
