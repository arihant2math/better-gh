//! Real api.github.com responses (`fixtures/github-recorded/`, captured
//! 2026-10-05 from the one repository this environment may read) carry
//! every field the importer reads, with the types it expects. Issue,
//! comment, event and release shapes are covered by the hand-written
//! fixtures (`fixtures/github/`) and by importing from this server's own
//! API (`self_import.rs`).

use serde_json::Value;

fn recorded(name: &str) -> Value {
    let path = format!(
        "{}/tests/it/fixtures/github-recorded/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

#[test]
fn repository_fields_the_importer_reads() {
    let repo = recorded("repo.json");
    // `create_import`: clone URL and default visibility.
    assert!(repo["clone_url"].as_str().unwrap().ends_with(".git"));
    assert!(matches!(
        repo["visibility"].as_str(),
        Some("public" | "private" | "internal")
    ));
    assert!(repo["private"].is_boolean());
    // Settings step.
    for key in ["description", "homepage"] {
        assert!(repo[key].is_string() || repo[key].is_null(), "{key}");
    }
    assert!(repo["topics"].is_array());
    for key in ["has_issues", "has_projects", "has_wiki", "has_discussions"] {
        assert!(repo[key].is_boolean(), "{key}");
    }
}

#[test]
fn label_fields_the_importer_reads() {
    let labels = recorded("labels.json");
    let labels = labels.as_array().unwrap();
    assert!(!labels.is_empty());
    for l in labels {
        assert!(l["name"].is_string());
        let color = l["color"].as_str().unwrap();
        assert!(color.len() == 6 && color.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(l["description"].is_string() || l["description"].is_null());
        assert!(l["default"].is_boolean());
    }
}
