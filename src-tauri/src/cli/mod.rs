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
