## ccs v3.16.0 Release Notes

### `ccs` — Claude CLI Switch

独立 CLI 工具，使用指定 provider 启动 Claude / Codex / Gemini / OpenCode，无需修改全局配置，支持多 provider 并发。

基于 [CC Switch](https://github.com/farion1231/cc-switch)。

### 功能

- `ccs claude <provider>` — 启动 Claude CLI（隔离 settings 文件 + 环境变量）
- `ccs codex <provider>` — 启动 Codex CLI
- `ccs gemini <provider>` — 启动 Gemini CLI
- `ccs opencode <provider>` — 启动 OpenCode CLI
- `ccs list [app]` — 列出所有可用 provider（`*` 标记当前激活）
- `ccs status [app]` — 显示各应用当前激活的 provider

### 使用示例

```bash
ccs claude my-relay              # 用指定 provider 启动 claude
ccs codex my-provider -- --help  # 透传参数给 codex
ccs list claude                  # 列出 claude 的所有 provider
ccs status                       # 查看各应用当前激活的 provider
```

### 特性

- 与 CC Switch GUI 共享同一数据库，GUI 中添加的 provider 立即可用
- Claude 使用临时 settings 文件 + 环境变量双重隔离
- Codex / Gemini / OpenCode 通过进程级环境变量注入
- 轻量独立二进制（~3 MB），不依赖 Tauri/WebView

### 下载

| 平台 | 文件 |
|------|------|
| Windows | `ccs-v3.16.0-Windows-x86_64.exe` |
| macOS (Universal) | `ccs-v3.16.0-macOS-universal` |
| Linux x86_64 | `ccs-v3.16.0-Linux-x86_64` |
| Linux ARM64 | `ccs-v3.16.0-Linux-arm64` |

下载后放到 `PATH` 中即可使用。macOS 需先执行 `chmod +x` 添加执行权限。

### macOS 安全提示

```bash
chmod +x ccs-v3.16.0-macOS-universal
xattr -d com.apple.quarantine ccs-v3.16.0-macOS-universal
./ccs-v3.16.0-macOS-universal --help
```
