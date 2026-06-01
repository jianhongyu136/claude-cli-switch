## ccs v3.16.1 Release Notes

### `ccs` — Claude CLI Switch

本次版本将 `ccs` 从“配置注入启动器”升级为支持**智能临时代理**的 provider-first CLI 启动器。

`ccs` 仍然不修改全局配置文件；当检测到选中的 provider 协议与目标 CLI 不匹配时，会为本次调用启动一个本地临时代理并自动转发/转换请求。

基于 [CC Switch](https://github.com/farion1231/cc-switch)。

### 重要变更：新的启动格式

从 v3.16.1 开始，启动命令使用 provider-first 格式：

```bash
ccs <provider> [ccs-options] <tool> [tool-args...]
```

示例：

```bash
ccs deepseek claude -p "hello"
ccs deepseek --no-proxy claude -p "hello"
ccs kimi codex exec "fix this"
ccs google gemini -p "hello"
ccs my-opencode-provider opencode run
```

参数区分规则：

- `ccs` 自己的参数必须放在 `<tool>` 之前。
- `<tool>` 之后的所有参数都会原样透传给目标 CLI。
- 因此：
  - `ccs deepseek --no-proxy claude -p "hello"` 表示禁用 `ccs` 自动代理。
  - `ccs deepseek claude --no-proxy` 表示把 `--no-proxy` 传给 `claude`。

### 自动临时代理

v3.16.1 首版支持 Claude / Codex / Gemini 的自动代理判断：

- Claude provider 如果是 OpenAI Chat / OpenAI Responses / Gemini Native / 托管账号等非 Anthropic 原生协议，会自动走临时代理转换为 Claude CLI 可用的 Anthropic Messages 协议。
- Codex provider 如果检测到 Chat Completions upstream，会自动走临时代理转换为 Codex CLI 期望的 Responses 使用方式。
- Gemini v1 先保持保守策略，原生 Gemini provider 直连。
- OpenCode v1 仍保持直连，不启用自动代理。

临时代理特点：

- 只影响当前 `ccs` 启动的子进程。
- 不修改 `~/.claude/settings.json`、`~/.codex/config.toml`、Gemini 配置或数据库当前 provider。
- 使用系统自动分配端口，支持多个 `ccs` 进程并发运行。
- 子进程退出后自动关闭临时代理并清理临时文件。

禁用自动代理：

```bash
ccs deepseek --no-proxy claude -p "hello"
ccs kimi --no-proxy codex exec "fix this"
ccs google --no-proxy gemini -p "hello"
```

### 管理命令保持不变

```bash
ccs list
ccs list claude
ccs status
ccs status codex
ccs --version
ccs --help
```

Provider 查找仍然优先匹配 **id**，其次按**名称**匹配（不区分大小写）。如果名称有歧义，请使用 provider id。

### 退出码

| 退出码 | 含义 |
|--------|------|
| 0      | 成功（目标 CLI 正常退出） |
| 2      | 数据库、配置或运行时初始化错误 |
| 3      | Provider 未找到 |
| 4      | Provider 名称有歧义 |
| 5      | Provider 没有可用环境变量配置 |
| 6      | 需要自动代理，但临时代理启动或配置生成失败 |
| 127    | 无法启动目标 CLI（不在 PATH 中） |

### 下载

| 平台 | 文件 |
|------|------|
| Windows | `ccs-v3.16.1-Windows-x86_64.exe` |
| macOS (Universal) | `ccs-v3.16.1-macOS-universal` |
| Linux x86_64 | `ccs-v3.16.1-Linux-x86_64` |
| Linux ARM64 | `ccs-v3.16.1-Linux-arm64` |

下载后放到 `PATH` 中即可使用。macOS 需先执行 `chmod +x` 添加执行权限。

### macOS 安全提示

```bash
chmod +x ccs-v3.16.1-macOS-universal
xattr -d com.apple.quarantine ccs-v3.16.1-macOS-universal
./ccs-v3.16.1-macOS-universal --help
```
