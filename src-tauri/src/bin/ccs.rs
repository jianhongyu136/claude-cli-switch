use cc_switch_lib::app_config::AppType;
use cc_switch_lib::cli::{extract_env_vars, find_provider, write_settings_file, CliError};
use cc_switch_lib::database::Database;
use clap::{Parser, Subcommand};
use std::process::Command;

#[derive(Parser)]
#[command(name = "ccs", version, about = "cc-switch CLI: launch Claude/Codex/Gemini with a chosen provider")]
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
}

fn main() {
    let cli = Cli::parse();
    let exit_code = match cli.command {
        Commands::Claude { provider, forward } => run_claude(&provider, &forward),
    };
    std::process::exit(exit_code);
}

fn run_claude(query: &str, forward: &[String]) -> i32 {
    let db = match Database::init() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ccs: failed to open cc-switch database: {e}");
            return 2;
        }
    };

    let app_type = AppType::Claude;
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
        eprintln!("ccs: provider '{}' has no env config — cannot launch claude", provider.name);
        return 5;
    }

    let temp_dir = std::env::temp_dir();
    let settings_path = temp_dir.join(format!(
        "ccs_claude_{}_{}.json",
        sanitize_filename(&provider.id),
        std::process::id()
    ));
    if let Err(e) = write_settings_file(&settings_path, &env_vars) {
        eprintln!("ccs: {e}");
        return 2;
    }

    let mut cmd = Command::new("claude");
    cmd.arg("--settings").arg(&settings_path);
    for arg in forward {
        cmd.arg(arg);
    }
    for (k, v) in &env_vars {
        cmd.env(k, v);
    }

    let status = cmd.status();
    let _ = std::fs::remove_file(&settings_path);

    match status {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => {
            eprintln!("ccs: failed to spawn claude: {e}. Is the claude CLI on your PATH?");
            127
        }
    }
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}
