use super::*;
use crate::app_config::AppType;
use crate::database::Database;
use crate::provider::Provider;
use serde_json::json;

fn make_test_db() -> Database {
    std::env::set_var(
        "CC_SWITCH_TEST_HOME",
        tempfile::tempdir()
            .unwrap()
            .keep()
            .to_string_lossy()
            .to_string(),
    );
    Database::init().expect("init db")
}

fn make_provider(id: &str, name: &str, base_url: &str, key: &str) -> Provider {
    Provider {
        id: id.to_string(),
        name: name.to_string(),
        settings_config: json!({
            "env": {
                "ANTHROPIC_BASE_URL": base_url,
                "ANTHROPIC_AUTH_TOKEN": key,
            }
        }),
        website_url: None,
        category: None,
        created_at: None,
        sort_index: None,
        notes: None,
        meta: None,
        icon: None,
        icon_color: None,
        in_failover_queue: false,
    }
}

#[test]
fn find_provider_matches_by_exact_id() {
    let db = make_test_db();
    let p = make_provider("abc123", "MyProvider", "https://api.example.com", "sk-test");
    db.save_provider("claude", &p).unwrap();

    let found = find_provider(&db, &AppType::Claude, "abc123").unwrap();
    assert_eq!(found.id, "abc123");
}

#[test]
fn find_provider_matches_by_exact_name() {
    let db = make_test_db();
    let p = make_provider("abc123", "MyProvider", "https://api.example.com", "sk-test");
    db.save_provider("claude", &p).unwrap();

    let found = find_provider(&db, &AppType::Claude, "MyProvider").unwrap();
    assert_eq!(found.id, "abc123");
}

#[test]
fn find_provider_is_case_insensitive_on_name() {
    let db = make_test_db();
    let p = make_provider("abc123", "MyProvider", "https://api.example.com", "sk-test");
    db.save_provider("claude", &p).unwrap();

    let found = find_provider(&db, &AppType::Claude, "myprovider").unwrap();
    assert_eq!(found.id, "abc123");
}

#[test]
fn find_provider_returns_error_when_missing() {
    let db = make_test_db();
    let err = find_provider(&db, &AppType::Claude, "does-not-exist").unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn find_provider_errors_on_ambiguous_name() {
    let db = make_test_db();
    let p1 = make_provider("id1", "Same", "https://a.example.com", "sk-1");
    let p2 = make_provider("id2", "Same", "https://b.example.com", "sk-2");
    db.save_provider("claude", &p1).unwrap();
    db.save_provider("claude", &p2).unwrap();

    let err = find_provider(&db, &AppType::Claude, "Same").unwrap_err();
    assert!(err.to_string().contains("ambiguous") || err.to_string().contains("multiple"));
}

#[test]
fn extract_env_vars_pulls_anthropic_block_for_claude() {
    let cfg = json!({
        "env": {
            "ANTHROPIC_AUTH_TOKEN": "sk-test",
            "ANTHROPIC_BASE_URL": "https://api.example.com"
        }
    });
    let vars = extract_env_vars(&cfg, &AppType::Claude);
    let map: std::collections::HashMap<_, _> = vars.into_iter().collect();
    assert_eq!(map.get("ANTHROPIC_AUTH_TOKEN").unwrap(), "sk-test");
    assert_eq!(map.get("ANTHROPIC_BASE_URL").unwrap(), "https://api.example.com");
}

#[test]
fn extract_env_vars_returns_empty_for_null_config() {
    let vars = extract_env_vars(&serde_json::Value::Null, &AppType::Claude);
    assert!(vars.is_empty());
}

#[test]
fn extract_env_vars_handles_codex_auth_field() {
    let cfg = json!({ "auth": "sk-codex" });
    let vars = extract_env_vars(&cfg, &AppType::Codex);
    assert_eq!(vars, vec![("OPENAI_API_KEY".to_string(), "sk-codex".to_string())]);
}

#[test]
fn write_settings_file_serializes_env_block() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test_settings.json");
    let env_vars = vec![
        ("ANTHROPIC_AUTH_TOKEN".to_string(), "sk-test".to_string()),
        ("ANTHROPIC_BASE_URL".to_string(), "https://x.example.com".to_string()),
    ];
    write_settings_file(&path, &env_vars).unwrap();

    let raw = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed["env"]["ANTHROPIC_AUTH_TOKEN"], "sk-test");
    assert_eq!(parsed["env"]["ANTHROPIC_BASE_URL"], "https://x.example.com");
}
