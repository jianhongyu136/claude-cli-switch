## ccs v3.16.3 Release Notes

### `ccs` — Claude CLI Switch

本版本同步了上游 [CC Switch](https://github.com/farion1231/cc-switch) v3.16.3 的全部改动，并把对 `ccs` 启动路径有实际影响的代理与协议转换修复带入独立 CLI。

`ccs` 是一个独立命令行工具，使用指定 provider 的环境启动 Claude / Codex / Gemini / OpenCode CLI，且**不修改全局配置文件**。自 v3.16.1 起，当所选 provider 的协议与目标 CLI 协议不匹配时（例如用 OpenAI Chat / Responses / Gemini 原生 provider 启动 Claude CLI），`ccs` 会为本次调用自动启动一个临时本地代理完成协议转换。

v3.16.3 在保持这一行为不变的前提下，主要强化了代理核心：自定义 User-Agent、Codex 图片与 Chat Completions 路由、格式转换时的 SSE 容错，以及与 GUI 共享的模型定价数据。

基于 [CC Switch](https://github.com/farion1231/cc-switch)。

### 适用场景

如果你通过 `ccs` 以第三方 provider 启动 CLI，并且命中以下任一情况，建议升级到 v3.16.3：

- provider 上游会按 User-Agent 做白名单校验（coding-plan / 中转类上游常见）；
- 用 `ccs <provider> codex ...` 启动 Codex，且 provider 实际走 OpenAI Chat Completions 或纯文本模型；
- 用 `ccs <provider> claude ...` 启动 Claude CLI，上游为 OpenAI Chat / Responses / Gemini 原生协议；
- 关注与 GUI 共享数据库中的用量计费与模型定价准确性。

```bash
ccs <provider> claude -p "hello"
ccs <provider> codex
ccs <provider> gemini
ccs <provider> opencode
```

### 新增内容

#### 自定义 User-Agent 覆盖

provider 配置现在可以设置自定义 User-Agent，代理会在请求转发、连通性检测、模型列表（`GET /v1/models`）中一致地应用该 UA。对于按 UA 做白名单的 coding-plan 上游，这能避免“代理本身能通、但检测或列表请求被 403 拦截”的问题。

由于 `ccs` 与 CC Switch GUI 共享同一个 SQLite 数据库（`~/.cc-switch/cc-switch.db`），在 GUI 中为 provider 配置的自定义 UA 会被 `ccs` 的临时代理直接使用，无需在 CLI 侧重复配置。

#### Codex 图片自适应（/responses 纯文本上游）

当 Codex `/responses` 请求携带图片、却被路由到纯文本 OpenAI-chat 模型（例如 DeepSeek `deepseek-v4-flash`）时，不再因 `unknown variant image_url` 报 HTTP 400。媒体纠错器（media sanitizer）现在也覆盖 Codex 适配器：会扫描 responses `input` 中的 `input_image` 块，对已知纯文本模型主动剥离图片，并在上游拒绝图片输入时按替换重试。

这直接影响 `ccs <provider> codex ...` 在 Chat 路由或纯文本模型下的稳定性。

#### 与上游 v3.16.3 同步

本次合入了上游 v3.16.2 / v3.16.3 的全部改动，其中对 `ccs` 透明生效的包括：

- **模型定价数据刷新**：新增 9 个模型定价（含 Claude Fable 5、Grok 4.3、Mistral Medium 3.5 / Small 4、Qwen 3.7 Max/Plus 等），并校正 28 个已有模型的价格；`ccs` 与 GUI 共享同一份定价种子，用量成本估算更准确。
- **新增模型档位与定价**：注册了 `claude-fable-5` 模型档位（fable → opus → default 回退）以及 `claude-mythos-5`、`kimi-k2.7-code` 的定价。
- **新增 provider 预设**：Unity2.ai（七端通用）、恢复 Codex “Kimi For Coding” 预设（配合自定义 UA 绕过 403）等，均可被 `ccs list` 列出并使用。

### 修复内容

#### 格式转换路径上的 SSE 容错

当 Claude/Codex 格式转换请求经过 MaaS 网关时，部分网关会对 `stream:false` 请求强制返回 SSE 体，却带非 SSE 的 Content-Type，此前会以一个含糊的 422 “Failed to parse upstream response” 失败。代理现在会在解析失败时嗅探 SSE，把多个 chunk 聚合成单个 JSON 再走既有转换器，客户端仍能拿到合法的非流式响应；剩余的解析失败也会附带 content-type、编码与 body 片段诊断，deflate 解码则先试 zlib 再试 raw。

这对 `ccs <provider> claude ...` / `ccs <provider> codex ...` 在中转网关下的成功率有直接改善。

#### Codex Chat Completions 路由强化

`ccs <provider> codex ...` 在 provider 走 OpenAI Chat Completions 时，合入了上游对 Codex Chat 路由的多项修复：

- 上游在无 `finish_reason` / `[DONE]` 结束流时，不再误判为正常完成：有部分输出时发出 `max_output_tokens` 不完整响应，无输出时发出 `stream_truncated` 失败事件；
- 结果 `tools` 为空时丢弃 `tool_choice` 与 `parallel_tool_calls`，避免严格上游以 “`tool_choice` requires `tools`” 拒绝；
- 自定义工具（如 `apply_patch`）保留完整原始定义进入 Chat function description；
- Chat 转 Responses 的 usage 始终包含 `output_tokens_details.reasoning_tokens`，满足 Codex CLI 的严格解析；
- 跨轮 reasoning 缓存覆盖 `function_call` / `custom_tool_call` / `tool_search_call` 全部工具类型。

#### 用量计费按真实上游模型

代理此前在路由接管（env 模型映射、Claude Desktop 路由、Copilot 归一化、Codex chat 覆盖）时，会按上游回显的模型计费，导致 kimi/glm 的 token 被记成 `claude-*`、成本被高估约 5–25×。转发器现在会捕获真实的出站模型，按“上游回显 → 出站 → 客户端别名”的顺序归属用量，并在每一行持久化真实计费基准（schema v11）。`ccs` 与 GUI 共享同一数据库，因此这部分计费修正对 `ccs` 启动的会话同样生效。

#### 格式转换路径的 token / 缓存核算

审计并修复了代理格式转换路径（Chat、Responses、Gemini 转 Anthropic）的 token 与缓存核算：记录实际返回模型、注入 `stream_options.include_usage`、在 Claude←OpenAI 路径上把 `cache_read` / `cache_creation` 从 input 中剔除以避免缓存 token 双计、扣除 Gemini 缓存的 prompt token、仍记录全缓存请求、并跳过此前会虚增请求计数的全零合成 usage。

### 兼容性说明

- 本次为同步上游 + 代理核心强化，`ccs` 的命令行用法、临时代理机制、`--no-proxy` 直连模式均无变化。
- Anthropic 原生 provider、以及不经过协议转换的 Codex / Gemini / OpenCode 启动路径，行为与 v3.16.2 一致。
- 自定义 User-Agent 为可选配置；未配置时代理行为与之前相同。
- 与 CC Switch GUI 共享 `~/.cc-switch/cc-switch.db`，provider、定价、用量数据互通。

### 下载

| 平台 | 文件 |
|------|------|
| Windows | `ccs-v3.16.3-Windows-x86_64.exe` |
| macOS (Universal) | `ccs-v3.16.3-macOS-universal` |
| Linux x86_64 | `ccs-v3.16.3-Linux-x86_64` |
| Linux ARM64 | `ccs-v3.16.3-Linux-arm64` |

下载后放到 `PATH` 中即可使用。macOS 需先执行 `chmod +x` 添加执行权限。

### macOS 安全提示

```bash
chmod +x ccs-v3.16.3-macOS-universal
xattr -d com.apple.quarantine ccs-v3.16.3-macOS-universal
./ccs-v3.16.3-macOS-universal --help
```
