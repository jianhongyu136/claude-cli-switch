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

## Why ccs?

- **No global config mutation** — Each invocation uses a temp settings file + process-level env vars; `~/.claude/settings.json` stays untouched
- **Concurrent multi-provider** — Run Claude with Provider A in one terminal and Provider B in another, simultaneously
- **Zero extra setup** — Shares the same SQLite database as the CC Switch GUI (`~/.cc-switch/cc-switch.db`); any provider you add in the GUI is available to `ccs` immediately
- **Lightweight** — ~3 MB standalone binary, no Tauri/WebView dependency

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
cargo build --release --bin ccs
# Binary at: target/release/ccs (or ccs.exe on Windows)
```

## Usage

```bash
# Launch Claude / Codex / Gemini / OpenCode
ccs claude <provider-name-or-id>
ccs codex <provider-name-or-id>
ccs gemini <provider-name-or-id>
ccs opencode <provider-name-or-id>

# Forward args to the CLI tool
ccs claude my-provider -- --help
ccs claude my-provider -- -p "hello"

# List all providers
ccs list              # all apps
ccs list claude       # claude only

# Show currently active provider
ccs status

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
| 127  | Failed to spawn target CLI (not on PATH) |

## How It Works

1. Reads provider config from `~/.cc-switch/cc-switch.db` (shared with CC Switch GUI)
2. Extracts the provider's environment variables (API Key, Base URL, etc.)
3. For Claude: writes a temp settings file and passes it via `--settings`
4. For Codex/Gemini: injects env vars at the process level
5. Launches the target CLI; cleans up temp files on exit

## Prerequisites

- [CC Switch](https://github.com/farion1231/cc-switch) GUI installed with at least one provider configured
- Target CLI tool installed and on PATH (`claude`, `codex`, `gemini`, or `opencode`)

## Roadmap

- [x] `ccs claude <provider>` — Launch Claude CLI
- [x] `ccs codex <provider>` — Launch Codex CLI
- [x] `ccs gemini <provider>` — Launch Gemini CLI
- [x] `ccs opencode <provider>` — Launch OpenCode CLI
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
cargo test --lib cli::tests

# Release build
cargo build --release --bin ccs
```

## Acknowledgments

This project is based on [CC Switch](https://github.com/farion1231/cc-switch) by [@farion1231](https://github.com/farion1231), reusing its provider database and environment variable extraction logic. Thanks to the CC Switch project for providing the infrastructure for the AI CLI tool ecosystem.

## License

MIT
