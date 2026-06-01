use cc_switch_lib::app_config::AppType;
use cc_switch_lib::cli::{extract_env_vars, find_provider, write_settings_file, CliError};
use cc_switch_lib::database::Database;
use clap::{Parser, Subcommand};
use std::process::Command;

#[derive(Parser)]
#[command(name = "ccs", version, about = "cc-switch CLI: launch Claude/Codex/Gemini/OpenCode with a chosen provider")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch Claude CLI with the named provider's environment.
    Claude {
        /// Provider name or id (case-insensitive name match)
        provider: String,
        /// Extra args forwarded to the claude binary
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        forward: Vec<String>,
    },
    /// Launch Codex CLI with the named provider's environment.
    Codex {
        /// Provider name or id (case-insensitive name match)
        provider: String,
        /// Extra args forwarded to the codex binary
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        forward: Vec<String>,
    },
    /// Launch Gemini CLI with the named provider's environment.
    Gemini {
        /// Provider name or id (case-insensitive name match)
        provider: String,
        /// Extra args forwarded to the gemini binary
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        forward: Vec<String>,
    },
    /// Launch OpenCode CLI with the named provider's environment.
    Opencode {
        /// Provider name or id (case-insensitive name match)
        provider: String,
        /// Extra args forwarded to the opencode binary
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        forward: Vec<String>,
    },
    /// List all providers for a given app (or all apps).
    List {
        /// App name: claude, codex, gemini, opencode (omit to list all)
        app: Option<String>,
    },
    /// Show the currently active provider for each app.
    Status {
        /// App name: claude, codex, gemini, opencode (omit to show all)
        app: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let exit_code = match cli.command {
        Commands::Claude { provider, forward } => {
            run_tool(&provider, &forward, AppType::Claude, "claude")
        }
        Commands::Codex { provider, forward } => {
            run_tool(&provider, &forward, AppType::Codex, "codex")
        }
        Commands::Gemini { provider, forward } => {
            run_tool(&provider, &forward, AppType::Gemini, "gemini")
        }
        Commands::Opencode { provider, forward } => {
            run_tool(&provider, &forward, AppType::OpenCode, "opencode")
        }
        Commands::List { app } => run_list(app.as_deref()),
        Commands::Status { app } => run_status(app.as_deref()),
    };
    std::process::exit(exit_code);
}

fn run_tool(query: &str, forward: &[String], app_type: AppType, bin_name: &str) -> i32 {
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

    let env_vars = extract_env_vars(&provider.settings_config, &app_type);
    if env_vars.is_empty() {
        eprintln!(
            "ccs: provider '{}' has no env config — cannot launch {bin_name}",
            provider.name
        );
        return 5;
    }

    let mut cmd = Command::new(bin_name);

    // Claude supports --settings for isolated config file
    let settings_path = if app_type == AppType::Claude {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join(format!(
            "ccs_claude_{}_{}.json",
            sanitize_filename(&provider.id),
            std::process::id()
        ));
        if let Err(e) = write_settings_file(&path, &env_vars) {
            eprintln!("ccs: {e}");
            return 2;
        }
        cmd.arg("--settings").arg(&path);
        Some(path)
    } else {
        None
    };

    for arg in forward {
        cmd.arg(arg);
    }
    for (k, v) in &env_vars {
        cmd.env(k, v);
    }

    let status = cmd.status();

    if let Some(path) = &settings_path {
        let _ = std::fs::remove_file(path);
    }

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!(
                "ccs: failed to spawn {bin_name}: {e}. Is {bin_name} on your PATH?"
            );
            127
        }
    }
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
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
