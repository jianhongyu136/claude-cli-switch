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
