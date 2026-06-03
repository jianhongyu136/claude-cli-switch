## ccs v3.16.2 Release Notes

### `ccs` — Claude CLI Switch

本版本是一个针对 Claude Code + OpenAI Responses 自动代理路径的稳定性补丁。

当你使用 `ccs <provider> claude ...`，并且所选 Claude provider 实际走 `openai_responses` 协议时，`ccs` 会继续为本次调用启动临时本地代理，把 Claude Code 的 Anthropic Messages 请求转换到 OpenAI Responses，再把上游流式响应转换回 Claude Code 可理解的 Anthropic SSE。

v3.16.2 重点修复两个会直接影响 Claude Code 交互体验的问题：模型思考没有稳定显示，以及上游先返回工具调用、后返回文本时，Agent 可能误以为当前轮次已经结束。

基于 [CC Switch](https://github.com/farion1231/cc-switch)。

### 适用场景

如果你的 Claude provider 使用 OpenAI Responses 上游，例如 GPT-5.5 / GPT-5 系列 reasoning 模型，并通过以下形式启动 Claude Code：

```bash
ccs <provider> claude -p "hello"
ccs <provider> claude
```

建议升级到 v3.16.2。

### 修复内容

#### Claude Code 现在可以看到 OpenAI Responses 思考

对于支持 reasoning effort 的 OpenAI Responses 模型，`ccs` 现在会默认请求：

```json
{
  "reasoning": {
    "effort": "high",
    "summary": "auto"
  }
}
```

这让上游能够返回 reasoning summary 事件，代理再把它们转换成 Claude Code 可显示的 thinking block。此前如果 Claude Code 的原始请求没有显式包含 `thinking` 配置，部分 Responses 模型不会返回 summary，导致 Claude Code 看不到模型进入思考。

如果请求显式关闭 thinking，`ccs` 仍会尊重该设置，不会强制请求 reasoning summary。

#### 多段 Responses thinking 合并为一个 Claude thinking block

OpenAI Responses 可能对同一个 reasoning item 返回多个 summary part。v3.16.2 会把同一个 reasoning item 下的多段 summary 合并到一个 Claude thinking block 中，而不是让 Claude Code 显示多个分散的 think 块。

这让 Claude Code 中的思考展示更接近一次完整推理过程，也避免多段 summary 被误认为多次独立思考。

#### 工具调用不会再抢在文本前导致 Agent 暂停

部分 OpenAI Responses 上游会先流出 `function_call` / 工具参数，再补发模型文本。Claude Code 在收到 tool-first 的转换结果时，可能把这一轮判断为工具调用已经结束，从而暂停等待。

v3.16.2 会在尚未看到文本内容时暂存早到的工具调用，等文本完成后再按 Anthropic SSE 顺序发给 Claude Code。这样可以保留模型回复，又能继续正常进入工具调用流程。

#### 保留多工具交错参数顺序

在暂存早到工具调用的同时，代理会保留多个工具调用之间交错到达的参数 delta 顺序。也就是说，即使上游以交错方式发送多个 `function_call_arguments.delta`，转换后的工具参数仍会按原始流式到达顺序重建。

### 兼容性说明

- 仅影响通过 `openai_responses` 协议代理到 Claude Code 的路径。
- Anthropic 原生 provider、OpenAI Chat Completions provider、Codex / Gemini / OpenCode 启动路径不受此补丁影响。
- 显式设置 `thinking: { "type": "disabled" }` 的请求会继续关闭 thinking。
- 对不支持 reasoning effort 的模型，不会额外注入 Responses reasoning 配置。

### 下载

| 平台 | 文件 |
|------|------|
| Windows | `ccs-v3.16.2-Windows-x86_64.exe` |
| macOS (Universal) | `ccs-v3.16.2-macOS-universal` |
| Linux x86_64 | `ccs-v3.16.2-Linux-x86_64` |
| Linux ARM64 | `ccs-v3.16.2-Linux-arm64` |

下载后放到 `PATH` 中即可使用。macOS 需先执行 `chmod +x` 添加执行权限。

### macOS 安全提示

```bash
chmod +x ccs-v3.16.2-macOS-universal
xattr -d com.apple.quarantine ccs-v3.16.2-macOS-universal
./ccs-v3.16.2-macOS-universal --help
```
