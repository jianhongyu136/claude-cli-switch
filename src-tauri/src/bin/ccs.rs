use cc_switch_lib::app_config::AppType;
use cc_switch_lib::cli::{find_provider, CliError};
use cc_switch_lib::database::Database;

#[derive(Debug, Clone, PartialEq, Eq)]
struct LaunchTarget {
    app_type: AppType,
    bin_name: &'static str,
}

impl LaunchTarget {
    const fn new(app_type: AppType, bin_name: &'static str) -> Self {
        Self { app_type, bin_name }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParsedCommand {
    Launch {
        provider: String,
        target: LaunchTarget,
        no_proxy: bool,
        forward: Vec<String>,
    },
    List { app: Option<String> },
    Status { app: Option<String> },
    Help,
    Version,
}

fn main() {
    let exit_code = match parse_args_from(std::env::args()) {
        Ok(ParsedCommand::Launch {
            provider,
            target,
            no_proxy,
            forward,
        }) => {
            let app_type = target.app_type;
            let no_proxy = no_proxy || app_type == AppType::OpenCode;
            run_tool(&provider, &forward, app_type, target.bin_name, no_proxy)
        }
        Ok(ParsedCommand::List { app }) => run_list(app.as_deref()),
        Ok(ParsedCommand::Status { app }) => run_status(app.as_deref()),
        Ok(ParsedCommand::Help) => {
            print_usage();
            0
        }
        Ok(ParsedCommand::Version) => {
            println!("ccs {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Err(message) => {
            eprintln!("ccs: {message}");
            eprintln!();
            print_usage();
            2
        }
    };
    std::process::exit(exit_code);
}

fn parse_args_from<I, S>(args: I) -> Result<ParsedCommand, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut args: Vec<String> = args.into_iter().map(Into::into).collect();
    if !args.is_empty() {
        args.remove(0);
    }

    let Some(first) = args.first().cloned() else {
        return Ok(ParsedCommand::Help);
    };

    match first.as_str() {
        "--help" | "-h" => return Ok(ParsedCommand::Help),
        "--version" | "-V" => return Ok(ParsedCommand::Version),
        "list" => {
            if args.len() > 2 {
                return Err("list accepts at most one app argument".to_string());
            }
            return Ok(ParsedCommand::List { app: args.get(1).cloned() });
        }
        "status" => {
            if args.len() > 2 {
                return Err("status accepts at most one app argument".to_string());
            }
            return Ok(ParsedCommand::Status { app: args.get(1).cloned() });
        }
        _ => {}
    }

    let provider = first;
    let mut no_proxy = false;
    let mut idx = 1;

    while idx < args.len() {
        if let Some(target) = parse_launch_target(&args[idx]) {
            let forward = args[(idx + 1)..].to_vec();
            return Ok(ParsedCommand::Launch {
                provider,
                target,
                no_proxy,
                forward,
            });
        }

        match args[idx].as_str() {
            "--no-proxy" => no_proxy = true,
            other if other.starts_with('-') => {
                return Err(format!(
                    "unknown ccs option before tool: {other}. Put target CLI args after the tool name."
                ));
            }
            other => {
                return Err(format!(
                    "unknown tool '{other}'. Supported tools: claude, codex, gemini, opencode"
                ));
            }
        }
        idx += 1;
    }

    Err("missing tool. Usage: ccs <provider> [--no-proxy] <claude|codex|gemini|opencode> [tool-args...]".to_string())
}

fn parse_launch_target(tool: &str) -> Option<LaunchTarget> {
    match tool {
        "claude" => Some(LaunchTarget::new(AppType::Claude, "claude")),
        "codex" => Some(LaunchTarget::new(AppType::Codex, "codex")),
        "gemini" => Some(LaunchTarget::new(AppType::Gemini, "gemini")),
        "opencode" => Some(LaunchTarget::new(AppType::OpenCode, "opencode")),
        _ => None,
    }
}

fn print_usage() {
    println!(
        "cc-switch CLI: launch Claude/Codex/Gemini/OpenCode with a chosen provider\n\n\
Usage:\n  ccs <provider> [ccs-options] <tool> [tool-args...]\n  ccs list [app]\n  ccs status [app]\n\n\
Examples:\n  ccs deepseek claude -p \"hello\"\n  ccs deepseek --no-proxy claude -p \"hello\"\n  ccs kimi codex exec \"fix this\"\n  ccs google gemini -p \"hello\"\n  ccs opencode-provider opencode run\n\n\
ccs-options (must appear before <tool>):\n  --no-proxy    Disable automatic temporary proxy\n\n\
Tools: claude, codex, gemini, opencode\nApps: claude, codex, gemini, opencode"
    );
}

fn run_tool(
    query: &str,
    forward: &[String],
    app_type: AppType,
    bin_name: &str,
    no_proxy: bool,
) -> i32 {
    let db = match Database::init() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ccs: failed to open cc-switch database: {e}");
            return 2;
        }
    };

    let provider = match find_provider(&db, &app_type, query) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ccs: {e}");
            return match e {
                CliError::NotFound(_) => 3,
                CliError::Ambiguous(_, _) => 4,
                _ => 2,
            };
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("ccs: failed to initialize async runtime: {e}");
            return 2;
        }
    };

    runtime.block_on(cc_switch_lib::cli::proxy::run_with_optional_proxy(
        std::sync::Arc::new(db),
        app_type,
        provider,
        bin_name,
        forward,
        no_proxy,
    ))
}

const SUPPORTED_APPS: &[(&str, fn() -> AppType)] = &[
    ("claude", || AppType::Claude),
    ("codex", || AppType::Codex),
    ("gemini", || AppType::Gemini),
    ("opencode", || AppType::OpenCode),
];

fn parse_app_filter(app: Option<&str>) -> Result<Vec<(&'static str, AppType)>, i32> {
    match app {
        None => Ok(SUPPORTED_APPS.iter().map(|(n, f)| (*n, f())).collect()),
        Some(name) => {
            let lower = name.to_lowercase();
            match SUPPORTED_APPS.iter().find(|(n, _)| *n == lower.as_str()) {
                Some((n, f)) => Ok(vec![(*n, f())]),
                None => {
                    eprintln!(
                        "ccs: unknown app '{}'. Supported: claude, codex, gemini, opencode",
                        name
                    );
                    Err(2)
                }
            }
        }
    }
}

fn run_list(app: Option<&str>) -> i32 {
    let apps = match parse_app_filter(app) {
        Ok(a) => a,
        Err(code) => return code,
    };

    let db = match Database::init() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ccs: failed to open cc-switch database: {e}");
            return 2;
        }
    };

    for (name, app_type) in &apps {
        let providers = match db.get_all_providers(app_type.as_str()) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("ccs: failed to list providers for {name}: {e}");
                continue;
            }
        };

        let current_id = db
            .get_current_provider(app_type.as_str())
            .ok()
            .flatten();

        if providers.is_empty() {
            println!("[{name}] (no providers)");
        } else {
            println!("[{name}]");
            for (_, p) in &providers {
                let marker = if current_id.as_deref() == Some(&p.id) {
                    " *"
                } else {
                    ""
                };
                println!("  {}{marker}  ({})", p.name, p.id);
            }
        }
    }
    0
}

fn run_status(app: Option<&str>) -> i32 {
    let apps = match parse_app_filter(app) {
        Ok(a) => a,
        Err(code) => return code,
    };

    let db = match Database::init() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ccs: failed to open cc-switch database: {e}");
            return 2;
        }
    };

    for (name, app_type) in &apps {
        let current_id = match db.get_current_provider(app_type.as_str()) {
            Ok(Some(id)) => id,
            Ok(None) => {
                println!("{name}: (none)");
                continue;
            }
            Err(e) => {
                eprintln!("ccs: failed to get status for {name}: {e}");
                continue;
            }
        };

        let provider_name = db
            .get_provider_by_id(&current_id, app_type.as_str())
            .ok()
            .flatten()
            .map(|p| p.name)
            .unwrap_or_else(|| current_id.clone());

        println!("{name}: {provider_name}  ({current_id})");
    }
    0
}

#[cfg(test)]
mod parser_tests {
    use super::*;

    #[test]
    fn parse_provider_first_launch_forwards_tool_args() {
        let parsed = parse_args_from(["ccs", "deepseek", "claude", "-p", "hello"])
            .expect("parse launch");

        assert_eq!(
            parsed,
            ParsedCommand::Launch {
                provider: "deepseek".to_string(),
                target: LaunchTarget::new(AppType::Claude, "claude"),
                no_proxy: false,
                forward: vec!["-p".to_string(), "hello".to_string()],
            }
        );
    }

    #[test]
    fn parse_no_proxy_before_tool_as_ccs_option() {
        let parsed = parse_args_from(["ccs", "deepseek", "--no-proxy", "claude", "-p", "hello"])
            .expect("parse launch");

        assert_eq!(
            parsed,
            ParsedCommand::Launch {
                provider: "deepseek".to_string(),
                target: LaunchTarget::new(AppType::Claude, "claude"),
                no_proxy: true,
                forward: vec!["-p".to_string(), "hello".to_string()],
            }
        );
    }

    #[test]
    fn parse_no_proxy_after_tool_as_forwarded_arg() {
        let parsed = parse_args_from(["ccs", "deepseek", "claude", "--no-proxy"])
            .expect("parse launch");

        assert_eq!(
            parsed,
            ParsedCommand::Launch {
                provider: "deepseek".to_string(),
                target: LaunchTarget::new(AppType::Claude, "claude"),
                no_proxy: false,
                forward: vec!["--no-proxy".to_string()],
            }
        );
    }

    #[test]
    fn parse_management_commands_remain_top_level() {
        assert_eq!(
            parse_args_from(["ccs", "list", "codex"]).expect("parse list"),
            ParsedCommand::List { app: Some("codex".to_string()) }
        );
        assert_eq!(
            parse_args_from(["ccs", "status", "gemini"]).expect("parse status"),
            ParsedCommand::Status { app: Some("gemini".to_string()) }
        );
    }
}
