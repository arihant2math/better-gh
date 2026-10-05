//! `GET /_bgh/repos/{o}/{r}/commits/{sha}/annotations` (inline check-run
//! annotations in the diff viewer).

use crate::common;

use common::*;
use serde_json::{Value, json};

#[tokio::test]
async fn commit_annotations_across_runs() {
    let f = fixture().await;
    let app = &f.app;
    let sha = f.feature.clone();
    let url = format!("/_bgh/repos/alice/demo/commits/{sha}/annotations");

    // No check runs yet.
    let res = app.get(&url).send().await;
    res.assert_status(200);
    assert_eq!(res.json(), json!([]));

    for (name, target, anns) in [
        (
            "lint",
            sha.as_str(),
            json!([
                {"path": "README.md", "start_line": 3, "end_line": 3, "annotation_level": "warning",
                 "message": "Trailing word", "title": "MD009", "raw_details": "details"},
                {"path": "README.md", "start_line": 1, "end_line": 2, "annotation_level": "notice",
                 "message": "Heading style"}
            ]),
        ),
        (
            "test",
            sha.as_str(),
            json!([{"path": "notes.txt", "start_line": 1, "end_line": 1, "annotation_level": "failure",
                    "message": "boom", "start_column": 2, "end_column": 4}]),
        ),
        (
            "other-commit",
            f.main.as_str(),
            json!([{"path": "README.md", "start_line": 1, "end_line": 1, "annotation_level": "failure",
                    "message": "not this commit"}]),
        ),
    ] {
        app.post("/api/v3/repos/alice/demo/check-runs")
            .auth(&f.alice)
            .json(&json!({
                "name": name, "head_sha": target, "status": "completed", "conclusion": "failure",
                "output": {"title": name, "summary": "s", "annotations": anns}
            }))
            .send()
            .await
            .assert_status(201);
    }

    let list: Value = app.get(&url).send().await.json();
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 3, "{list:?}");
    // Ordered by path, then line.
    assert_eq!(list[0]["path"], "README.md");
    assert_eq!(list[0]["start_line"], 1);
    assert_eq!(list[0]["end_line"], 2);
    assert_eq!(list[0]["annotation_level"], "notice");
    assert_eq!(list[0]["check_run_name"], "lint");
    assert!(list[0]["check_run_id"].is_i64());
    assert!(list[0]["title"].is_null());
    assert_eq!(list[1]["title"], "MD009");
    assert_eq!(list[1]["raw_details"], "details");
    assert_eq!(list[1]["message"], "Trailing word");
    assert_eq!(list[2]["path"], "notes.txt");
    assert_eq!(list[2]["check_run_name"], "test");
    assert_eq!(list[2]["start_column"], 2);
    assert_eq!(list[2]["end_column"], 4);

    // Bad SHA / unknown repository.
    app.get("/_bgh/repos/alice/demo/commits/main/annotations")
        .send()
        .await
        .assert_status(404);
    app.get(&format!("/_bgh/repos/alice/nope/commits/{sha}/annotations"))
        .send()
        .await
        .assert_status(404);
}
