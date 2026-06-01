use crate::app_config::AppType;
use crate::cli::{extract_env_vars, write_settings_file};
use crate::database::Database;
use crate::provider::Provider;
use crate::proxy::providers::{
    claude_api_format_needs_transform, codex_provider_upstream_model,
    codex_provider_uses_chat_completions, get_claude_api_format,
};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

const PROXY_TOKEN_PLACEHOLDER: &str = "PROXY_MANAGED";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyDecision {
    Direct,
    TemporaryProxy { reason: String },
}

struct ChildLaunchConfig {
    env_vars: Vec<(String, String)>,
    pre_args: Vec<String>,
    temp_paths: Vec<PathBuf>,
}

pub fn decide_proxy(app_type: &AppType, provider: &Provider, no_proxy: bool) -> ProxyDecision {
    if no_proxy {
        return ProxyDecision::Direct;
    }

    match app_type {
        AppType::Claude => decide_claude_proxy(provider),
        AppType::Codex => decide_codex_proxy(provider),
        AppType::Gemini => decide_gemini_proxy(provider),
        AppType::OpenCode | AppType::OpenClaw | AppType::Hermes | AppType::ClaudeDesktop => {
            ProxyDecision::Direct
        }
    }
}

pub async fn run_with_optional_proxy(
    db: Arc<Database>,
    app_type: AppType,
    provider: Provider,
    bin_name: &str,
    forward: &[String],
    no_proxy: bool,
) -> i32 {
    match decide_proxy(&app_type, &provider, no_proxy) {
        ProxyDecision::Direct => run_child_direct(&app_type, &provider, bin_name, forward),
        ProxyDecision::TemporaryProxy { reason } => {
            eprintln!(
                "ccs: auto proxy enabled for {} provider '{}' ({reason})",
                app_type.as_str(),
                provider.name
            );
            run_child_with_temporary_proxy(db, app_type, provider, bin_name, forward).await
        }
    }
}

fn decide_claude_proxy(provider: &Provider) -> ProxyDecision {
    let api_format = get_claude_api_format(provider);
    if claude_api_format_needs_transform(api_format) || provider.uses_managed_account_auth() {
        return ProxyDecision::TemporaryProxy {
            reason: format!("Claude provider requires {api_format} conversion"),
        };
    }
    ProxyDecision::Direct
}

fn decide_codex_proxy(provider: &Provider) -> ProxyDecision {
    if codex_provider_uses_chat_completions(provider) {
        return ProxyDecision::TemporaryProxy {
            reason: "Codex provider uses OpenAI Chat Completions upstream".to_string(),
        };
    }
    ProxyDecision::Direct
}

fn decide_gemini_proxy(_provider: &Provider) -> ProxyDecision {
    // First version stays conservative for Gemini unless an explicit future marker is added.
    ProxyDecision::Direct
}

fn run_child_direct(
    app_type: &AppType,
    provider: &Provider,
    bin_name: &str,
    forward: &[String],
) -> i32 {
    let env_vars = extract_env_vars(&provider.settings_config, app_type);
    if env_vars.is_empty() {
        eprintln!(
            "ccs: provider '{}' has no env config — cannot launch {bin_name}",
            provider.name
        );
        return 5;
    }

    let launch = ChildLaunchConfig {
        env_vars,
        pre_args: Vec::new(),
        temp_paths: Vec::new(),
    };
    run_child_with_config(app_type, provider, bin_name, forward, launch)
}

async fn run_child_with_temporary_proxy(
    db: Arc<Database>,
    app_type: AppType,
    provider: Provider,
    bin_name: &str,
    forward: &[String],
) -> i32 {
    let server = crate::proxy::temporary_cli_proxy_server(db, None);
    server
        .set_selected_provider_override(app_type.as_str(), provider.clone())
        .await;

    let info = match server.start().await {
        Ok(info) => info,
        Err(e) => {
            eprintln!("ccs: failed to start temporary proxy: {e}");
            return 6;
        }
    };

    let proxy_url = format!("http://{}:{}", info.address, info.port);
    eprintln!("ccs: local proxy listening on {proxy_url}");

    let launch = match build_proxy_launch_config(&app_type, &provider, &proxy_url) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("ccs: failed to prepare temporary proxy config: {e}");
            let _ = server.stop().await;
            return 6;
        }
    };

    let code = run_child_with_config(&app_type, &provider, bin_name, forward, launch);

    if let Err(e) = server.stop().await {
        eprintln!("ccs: warning: failed to stop temporary proxy: {e}");
    }

    code
}

fn build_proxy_launch_config(
    app_type: &AppType,
    provider: &Provider,
    proxy_url: &str,
) -> Result<ChildLaunchConfig, String> {
    let env_vars = build_proxy_env_vars(app_type, provider, proxy_url);
    let mut pre_args = Vec::new();
    let mut temp_paths = Vec::new();

    if *app_type == AppType::Codex {
        let dir = tempfile::Builder::new()
            .prefix("ccs_codex_proxy_")
            .tempdir()
            .map_err(|e| format!("create temp codex dir failed: {e}"))?
            .keep();
        let config_path = dir.join("config.toml");
        let auth_path = dir.join("auth.json");
        std::fs::write(
            &config_path,
            build_codex_proxy_config(
                proxy_url,
                codex_provider_upstream_model(provider).as_deref(),
            ),
        )
        .map_err(|e| format!("write codex config failed: {e}"))?;
        std::fs::write(
            &auth_path,
            format!(r#"{{"OPENAI_API_KEY":"{PROXY_TOKEN_PLACEHOLDER}"}}"#),
        )
        .map_err(|e| format!("write codex auth failed: {e}"))?;
        pre_args.extend([
            "--config".to_string(),
            "model_provider=\"ccs-local-proxy\"".to_string(),
            "--config".to_string(),
            format!(
                "model=\"{}\"",
                codex_provider_upstream_model(provider).unwrap_or_else(|| "gpt-4o".to_string())
            ),
        ]);
        temp_paths.push(config_path);
        temp_paths.push(auth_path);
        temp_paths.push(dir);
    }

    Ok(ChildLaunchConfig {
        env_vars,
        pre_args,
        temp_paths,
    })
}

fn build_proxy_env_vars(
    app_type: &AppType,
    provider: &Provider,
    proxy_url: &str,
) -> Vec<(String, String)> {
    match app_type {
        AppType::Claude => vec![
            ("ANTHROPIC_BASE_URL".to_string(), proxy_url.to_string()),
            (
                "ANTHROPIC_AUTH_TOKEN".to_string(),
                PROXY_TOKEN_PLACEHOLDER.to_string(),
            ),
            (
                "ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(),
                "claude-haiku-4-5".to_string(),
            ),
            (
                "ANTHROPIC_DEFAULT_SONNET_MODEL".to_string(),
                "claude-sonnet-4-6".to_string(),
            ),
            (
                "ANTHROPIC_DEFAULT_OPUS_MODEL".to_string(),
                "claude-opus-4-8".to_string(),
            ),
        ],
        AppType::Codex => vec![
            (
                "OPENAI_API_KEY".to_string(),
                PROXY_TOKEN_PLACEHOLDER.to_string(),
            ),
            (
                "CODEX_HOME".to_string(),
                std::env::temp_dir().to_string_lossy().to_string(),
            ),
        ],
        AppType::Gemini => {
            let mut vars = vec![
                ("GOOGLE_GEMINI_BASE_URL".to_string(), proxy_url.to_string()),
                (
                    "GEMINI_API_KEY".to_string(),
                    PROXY_TOKEN_PLACEHOLDER.to_string(),
                ),
            ];
            if let Some(model) = provider
                .settings_config
                .pointer("/env/GEMINI_MODEL")
                .and_then(|v| v.as_str())
                .or_else(|| {
                    provider
                        .settings_config
                        .get("model")
                        .and_then(|v| v.as_str())
                })
            {
                vars.push(("GEMINI_MODEL".to_string(), model.to_string()));
            }
            vars
        }
        _ => Vec::new(),
    }
}

fn build_codex_proxy_config(proxy_url: &str, model: Option<&str>) -> String {
    let base_url = format!("{}/v1", proxy_url.trim_end_matches('/'));
    let model = model.unwrap_or("gpt-4o");
    format!(
        r#"model_provider = "ccs-local-proxy"
model = "{model}"

[model_providers.ccs-local-proxy]
name = "ccs-local-proxy"
base_url = "{base_url}"
wire_api = "responses"
requires_openai_auth = true
"#
    )
}

fn run_child_with_config(
    app_type: &AppType,
    provider: &Provider,
    bin_name: &str,
    forward: &[String],
    launch: ChildLaunchConfig,
) -> i32 {
    let mut cmd = Command::new(bin_name);

    let settings_path = if *app_type == AppType::Claude {
        let path = std::env::temp_dir().join(format!(
            "ccs_claude_{}_{}.json",
            sanitize_filename(&provider.id),
            std::process::id()
        ));
        if let Err(e) = write_settings_file(&path, &launch.env_vars) {
            eprintln!("ccs: {e}");
            cleanup_paths(&launch.temp_paths);
            return 2;
        }
        cmd.arg("--settings").arg(&path);
        Some(path)
    } else {
        None
    };

    for arg in &launch.pre_args {
        cmd.arg(arg);
    }
    for arg in forward {
        cmd.arg(arg);
    }
    for (k, v) in &launch.env_vars {
        cmd.env(k, v);
    }

    let status = cmd.status();

    if let Some(path) = &settings_path {
        let _ = std::fs::remove_file(path);
    }
    cleanup_paths(&launch.temp_paths);

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("ccs: failed to spawn {bin_name}: {e}. Is {bin_name} on your PATH?");
            127
        }
    }
}

fn cleanup_paths(paths: &[PathBuf]) {
    for path in paths {
        if path.is_dir() {
            let _ = std::fs::remove_dir_all(path);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderMeta;
    use serde_json::{json, Value};

    fn provider_with_config(settings_config: Value, meta: Option<ProviderMeta>) -> Provider {
        Provider {
            id: "p1".to_string(),
            name: "Provider".to_string(),
            settings_config,
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
    }

    fn claude_provider_with_api_format(api_format: Option<&str>) -> Provider {
        let meta = api_format.map(|format| ProviderMeta {
            api_format: Some(format.to_string()),
            ..ProviderMeta::default()
        });
        provider_with_config(
            json!({ "env": { "ANTHROPIC_BASE_URL": "https://api.example.com" } }),
            meta,
        )
    }

    #[test]
    fn no_proxy_forces_direct() {
        let provider = claude_provider_with_api_format(Some("openai_chat"));
        assert_eq!(
            decide_proxy(&AppType::Claude, &provider, true),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn claude_anthropic_is_direct() {
        let provider = claude_provider_with_api_format(Some("anthropic"));
        assert_eq!(
            decide_proxy(&AppType::Claude, &provider, false),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn claude_missing_api_format_is_direct() {
        let provider = claude_provider_with_api_format(None);
        assert_eq!(
            decide_proxy(&AppType::Claude, &provider, false),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn claude_openai_chat_uses_proxy() {
        let provider = claude_provider_with_api_format(Some("openai_chat"));
        assert!(matches!(
            decide_proxy(&AppType::Claude, &provider, false),
            ProxyDecision::TemporaryProxy { .. }
        ));
    }

    #[test]
    fn claude_openai_responses_uses_proxy() {
        let provider = claude_provider_with_api_format(Some("openai_responses"));
        assert!(matches!(
            decide_proxy(&AppType::Claude, &provider, false),
            ProxyDecision::TemporaryProxy { .. }
        ));
    }

    #[test]
    fn claude_gemini_native_uses_proxy() {
        let provider = claude_provider_with_api_format(Some("gemini_native"));
        assert!(matches!(
            decide_proxy(&AppType::Claude, &provider, false),
            ProxyDecision::TemporaryProxy { .. }
        ));
    }

    #[test]
    fn claude_managed_account_provider_uses_proxy() {
        let provider = provider_with_config(
            json!({ "env": { "ANTHROPIC_BASE_URL": "https://api.githubcopilot.com" } }),
            Some(ProviderMeta {
                provider_type: Some("github_copilot".to_string()),
                ..ProviderMeta::default()
            }),
        );
        assert!(matches!(
            decide_proxy(&AppType::Claude, &provider, false),
            ProxyDecision::TemporaryProxy { .. }
        ));
    }

    #[test]
    fn codex_responses_wire_api_is_direct() {
        let provider = provider_with_config(
            json!({ "config": "model_provider = \"custom\"\n[model_providers.custom]\nwire_api = \"responses\"\n" }),
            None,
        );
        assert_eq!(
            decide_proxy(&AppType::Codex, &provider, false),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn codex_chat_wire_api_uses_proxy() {
        let provider = provider_with_config(
            json!({ "config": "model_provider = \"custom\"\n[model_providers.custom]\nwire_api = \"chat\"\n" }),
            None,
        );
        assert!(matches!(
            decide_proxy(&AppType::Codex, &provider, false),
            ProxyDecision::TemporaryProxy { .. }
        ));
    }

    #[test]
    fn gemini_native_env_is_direct() {
        let provider = provider_with_config(
            json!({ "api_key": "key", "env": { "GOOGLE_GEMINI_BASE_URL": "https://generativelanguage.googleapis.com" } }),
            None,
        );
        assert_eq!(
            decide_proxy(&AppType::Gemini, &provider, false),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn opencode_is_direct() {
        let provider = provider_with_config(json!({}), None);
        assert_eq!(
            decide_proxy(&AppType::OpenCode, &provider, false),
            ProxyDecision::Direct
        );
    }

    #[test]
    fn claude_proxy_env_points_to_local_proxy() {
        let env = build_proxy_env_vars(
            &AppType::Claude,
            &claude_provider_with_api_format(Some("openai_chat")),
            "http://127.0.0.1:49152",
        );
        let map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(
            map.get("ANTHROPIC_BASE_URL"),
            Some(&"http://127.0.0.1:49152".to_string())
        );
        assert_eq!(
            map.get("ANTHROPIC_AUTH_TOKEN"),
            Some(&"PROXY_MANAGED".to_string())
        );
    }

    #[test]
    fn codex_proxy_env_sets_openai_key_placeholder() {
        let provider = provider_with_config(json!({ "config": "model = \"gpt-test\"" }), None);
        let env = build_proxy_env_vars(&AppType::Codex, &provider, "http://127.0.0.1:49152");
        let map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(
            map.get("OPENAI_API_KEY"),
            Some(&"PROXY_MANAGED".to_string())
        );
    }

    #[test]
    fn codex_proxy_config_points_to_responses_proxy() {
        let toml = build_codex_proxy_config("http://127.0.0.1:49152", Some("gpt-test"));
        assert!(toml.contains("base_url = \"http://127.0.0.1:49152/v1\""));
        assert!(toml.contains("wire_api = \"responses\""));
        assert!(toml.contains("model = \"gpt-test\""));
    }

    #[test]
    fn gemini_proxy_env_points_to_local_proxy() {
        let provider = provider_with_config(json!({ "model": "gemini-test" }), None);
        let env = build_proxy_env_vars(&AppType::Gemini, &provider, "http://127.0.0.1:49152");
        let map: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(
            map.get("GOOGLE_GEMINI_BASE_URL"),
            Some(&"http://127.0.0.1:49152".to_string())
        );
        assert_eq!(
            map.get("GEMINI_API_KEY"),
            Some(&"PROXY_MANAGED".to_string())
        );
        assert_eq!(map.get("GEMINI_MODEL"), Some(&"gemini-test".to_string()));
    }
}
