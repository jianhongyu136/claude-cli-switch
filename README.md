<div align="center">

# ccs (Claude CLI Switch)

### Lightweight CLI tool: launch Claude / Codex / Gemini / OpenCode with a chosen provider

[![Version](https://img.shields.io/github/v/release/jianhongyu136/claude-cli-switch?color=blue&label=version)](https://github.com/jianhongyu136/claude-cli-switch/releases)
[![Platform](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey.svg)](https://github.com/jianhongyu136/claude-cli-switch/releases)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)](https://www.rust-lang.org/)

[English](README.md) | [中文](README_ZH.md)

</div>

> This project is based on [CC Switch](https://github.com/farion1231/cc-switch) (MIT license), extracting and extending its CLI capabilities into a standalone tool.

## What is ccs?

`ccs` is a standalone command-line tool that launches Claude / Codex / Gemini / OpenCode CLI with a chosen provider's environment — **without modifying global config files**. Each invocation is fully isolated, allowing you to run multiple terminals with different providers simultaneously.

Since v3.16.1, `ccs` can also automatically start a per-invocation temporary local proxy when the selected provider protocol does not match the target CLI protocol, such as launching Claude CLI with an OpenAI Chat / Responses / Gemini-native provider.

## Why ccs?

- **No global config mutation** — Each invocation uses temp settings/config files + process-level env vars; `~/.claude/settings.json` and other global CLI configs stay untouched
- **Automatic protocol routing** — Claude / Codex / Gemini launches can use a per-process temporary local proxy when provider protocol conversion is required
- **Concurrent multi-provider** — Run Claude with Provider A in one terminal and Provider B in another, simultaneously; temporary proxy ports are assigned by the OS to avoid conflicts
- **Zero extra setup** — Shares the same SQLite database as the CC Switch GUI (`~/.cc-switch/cc-switch.db`); any provider you add in the GUI is available to `ccs` immediately
- **Lightweight** — ~4.6 MB Windows binary with auto proxy support; default `ccs` build excludes Tauri/WebView dependencies

## Installation

Download the binary for your platform from the [Releases](https://github.com/jianhongyu136/claude-cli-switch/releases) page and place it anywhere on your `PATH`.

| Platform | File |
|----------|------|
| Windows | `ccs-v{version}-Windows-x86_64.exe` |
| macOS (Universal) | `ccs-v{version}-macOS-universal` |
| Linux x86_64 | `ccs-v{version}-Linux-x86_64` |
| Linux ARM64 | `ccs-v{version}-Linux-arm64` |

You can also build from source:

```bash
cd src-tauri
cargo build --profile release-ccs --bin ccs
# Binary at: target/release-ccs/ccs (or ccs.exe on Windows)
```

This standalone `ccs` build uses the default Cargo feature set and intentionally excludes Tauri/WebView dependencies. To build the full CC Switch desktop app, enable the GUI feature through the Tauri command:

```bash
pnpm tauri dev --features gui
pnpm tauri build --features gui
```

## Usage

```bash
# Launch Claude / Codex / Gemini / OpenCode with provider-first syntax
ccs <provider-name-or-id> claude
ccs <provider-name-or-id> codex
ccs <provider-name-or-id> gemini
ccs <provider-name-or-id> opencode

# Forward args to the target CLI tool
ccs my-provider claude -p "hello"
ccs my-provider codex exec "fix this"
ccs my-provider gemini -p "hello"

# Disable automatic temporary proxy (ccs options must appear before the tool name)
ccs my-provider --no-proxy claude -p "hello"
ccs my-provider --no-proxy codex exec "fix this"

# Args after the tool name belong to the target CLI
ccs my-provider claude --no-proxy   # forwards --no-proxy to claude

# List all providers
ccs list              # all apps
ccs list claude       # claude only

# Show currently active provider
ccs status
ccs status codex

# Version and help
ccs --version
ccs --help
```

Provider lookup matches by **id** first, then by **name** (case-insensitive). If a name is ambiguous (multiple providers share the same name), pass the provider id instead.

## Exit Codes

| Code | Meaning |
|------|---------|
| 0    | Success (CLI tool exited normally) |
| 2    | Database or settings file error |
| 3    | Provider not found |
| 4    | Ambiguous provider name |
| 5    | Provider has no env config |
| 6    | Temporary proxy startup or child proxy config generation failed |
| 127  | Failed to spawn target CLI (not on PATH) |

## How It Works

1. Reads provider config from `~/.cc-switch/cc-switch.db` (shared with CC Switch GUI)
2. Decides whether the selected provider can be used directly by the target CLI
3. If direct: extracts provider environment variables and launches the child process
4. If protocol conversion is required: starts a temporary local proxy on an OS-assigned port and points only the child process at it
5. For Claude: writes a temp settings file and passes it via `--settings`
6. For Codex/Gemini: injects process-level env vars and temporary proxy config when needed
7. Launches the target CLI; stops the temporary proxy and cleans up temp files on exit

## Prerequisites

- [CC Switch](https://github.com/farion1231/cc-switch) GUI installed with at least one provider configured
- Target CLI tool installed and on PATH (`claude`, `codex`, `gemini`, or `opencode`)

## Roadmap

- [x] `ccs <provider> claude` — Launch Claude CLI
- [x] `ccs <provider> codex` — Launch Codex CLI
- [x] `ccs <provider> gemini` — Launch Gemini CLI
- [x] `ccs <provider> opencode` — Launch OpenCode CLI
- [x] `ccs <provider> --no-proxy <tool>` — Force direct mode
- [x] Temporary auto proxy for Claude / Codex / Gemini protocol conversion
- [x] `ccs list [app]` — List all providers
- [x] `ccs status [app]` — Show currently active provider
- [ ] `ccs switch <app> <provider>` — Switch the active provider
- [ ] Shell completions (bash/zsh/fish/powershell)
- [ ] MCP server config forwarding

## Development

```bash
cd src-tauri

# Build
cargo build --bin ccs

# Run tests
cargo test --bin ccs parser_tests
cargo test --lib cli::tests
cargo test --lib cli::proxy::tests
cargo test --lib proxy::server::tests

# Release build
cargo build --profile release-ccs --bin ccs
```

## Acknowledgments

This project is based on [CC Switch](https://github.com/farion1231/cc-switch) by [@farion1231](https://github.com/farion1231), reusing its provider database and environment variable extraction logic. Thanks to the CC Switch project for providing the infrastructure for the AI CLI tool ecosystem.

## License

MIT
