use crate::app_config::AppType;
use crate::database::Database;
use crate::provider::Provider;
use std::fmt;

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub enum CliError {
    NotFound(String),
    Ambiguous(String, Vec<String>),
    Db(String),
    Spawn(String),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CliError::NotFound(q) => write!(f, "provider not found: {q}"),
            CliError::Ambiguous(q, ids) => write!(
                f,
                "ambiguous provider name {q} matched multiple providers: {}. Use the provider id instead.",
                ids.join(", ")
            ),
            CliError::Db(msg) => write!(f, "database error: {msg}"),
            CliError::Spawn(msg) => write!(f, "failed to spawn claude: {msg}"),
        }
    }
}

impl std::error::Error for CliError {}

pub fn find_provider(
    db: &Database,
    app_type: &AppType,
    query: &str,
) -> Result<Provider, CliError> {
    if let Some(p) = db
        .get_provider_by_id(query, app_type.as_str())
        .map_err(|e| CliError::Db(e.to_string()))?
    {
        return Ok(p);
    }

    let all = db
        .get_all_providers(app_type.as_str())
        .map_err(|e| CliError::Db(e.to_string()))?;

    let q_lower = query.to_lowercase();
    let matches: Vec<Provider> = all
        .into_iter()
        .filter(|(_, p)| p.name.to_lowercase() == q_lower)
        .map(|(_, p)| p)
        .collect();

    match matches.len() {
        0 => Err(CliError::NotFound(query.to_string())),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => Err(CliError::Ambiguous(
            query.to_string(),
            matches.into_iter().map(|p| p.id).collect(),
        )),
    }
}

pub fn extract_env_vars(
    config: &serde_json::Value,
    app_type: &AppType,
) -> Vec<(String, String)> {
    let mut env_vars = Vec::new();

    let Some(obj) = config.as_object() else {
        return env_vars;
    };

    if let Some(env) = obj.get("env").and_then(|v| v.as_object()) {
        for (key, value) in env {
            if let Some(str_val) = value.as_str() {
                env_vars.push((key.clone(), str_val.to_string()));
            }
        }

        let base_url_key = match app_type {
            AppType::Claude | AppType::ClaudeDesktop => Some("ANTHROPIC_BASE_URL"),
            AppType::Gemini => Some("GOOGLE_GEMINI_BASE_URL"),
            _ => None,
        };

        if let Some(key) = base_url_key {
            if let Some(url_str) = env.get(key).and_then(|v| v.as_str()) {
                env_vars.push((key.to_string(), url_str.to_string()));
            }
        }
    }

    if *app_type == AppType::Codex {
        if let Some(auth) = obj.get("auth").and_then(|v| v.as_str()) {
            env_vars.push(("OPENAI_API_KEY".to_string(), auth.to_string()));
        }
    }

    if *app_type == AppType::Gemini {
        if let Some(api_key) = obj.get("api_key").and_then(|v| v.as_str()) {
            env_vars.push(("GEMINI_API_KEY".to_string(), api_key.to_string()));
        }
    }

    env_vars
}
