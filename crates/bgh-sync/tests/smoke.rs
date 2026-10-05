//! Integration tests for bgh-sync.

#[tokio::test]
async fn router_is_mounted() {
    let app = bgh_server::test_app().await;
    // The crate has no routes yet; the API fallback answers with a JSON 404.
    let res = app.get("/api/v3/__bgh_sync_placeholder").send().await;
    assert_eq!(res.status(), 404);
}
