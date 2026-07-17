//! Integration tests for `POST /admin/delete-page`.

use ai_memory_core::{PagePath, ProjectId, Tier, WorkspaceId};
use ai_memory_mcp::{AdminState, admin_router};
use ai_memory_store::{DecayParams, Store};
use ai_memory_wiki::{Wiki, WritePageRequest};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

async fn make_state(tmp: &TempDir) -> (AdminState, Store) {
    let store = Store::open(tmp.path()).unwrap();
    let wiki = Wiki::new(tmp.path(), store.writer.clone()).unwrap();
    let db_path = store.db_path().to_path_buf();
    let state = AdminState {
        writer: store.writer.clone(),
        reader: store.reader.clone(),
        wiki,
        llm: None,
        auto_improve_require_approval: false,
        auto_improve_review_config: Default::default(),
        embedder: None,
        provider_health: ai_memory_llm::ProviderHealth::default(),
        decay_params: DecayParams::default(),
        data_dir: tmp.path().to_path_buf(),
        bind: "127.0.0.1:0".to_string(),
        home_dir: None,
        bootstrap_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        token_pepper: None,
        active_project: ai_memory_core::ActiveProject::new(),
        scope_invalidator: None,
        db_path,
    };
    (state, store)
}

async fn post(state: AdminState, uri: &str, body: serde_json::Value) -> axum::response::Response {
    let router = admin_router(state);
    let req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    router.oneshot(req).await.unwrap()
}

async fn seed_page(state: &AdminState) -> (WorkspaceId, ProjectId, String) {
    let ws = state
        .writer
        .get_or_create_workspace("default")
        .await
        .unwrap();
    let proj = state
        .writer
        .get_or_create_project(ws, "scratch", None)
        .await
        .unwrap();
    let path = "notes/doomed.md".to_string();
    state
        .wiki
        .write_page(WritePageRequest {
            workspace_id: ws,
            project_id: proj,
            path: PagePath::new(&path).unwrap(),
            frontmatter: serde_json::json!({}),
            body: "delete me".into(),
            tier: Tier::Semantic,
            pinned: false,
            title: Some("doomed".into()),
            admission_ctx: None,
            author_id: None,
            actor: ai_memory_core::ActorContext::anonymous(),
        })
        .await
        .unwrap();
    (ws, proj, path)
}

#[tokio::test]
async fn delete_page_without_confirm_succeeds() {
    let tmp = TempDir::new().unwrap();
    let (state, _store) = make_state(&tmp).await;
    let (_ws, _proj, path) = seed_page(&state).await;

    let resp = post(
        state,
        "/admin/delete-page",
        json!({ "workspace": "default", "project": "scratch", "path": path }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn delete_page_with_confirm_removes_page() {
    let tmp = TempDir::new().unwrap();
    let (state, _store) = make_state(&tmp).await;
    let (ws, proj, path) = seed_page(&state).await;

    // On-disk file path (compute before the state move into `post`).
    let file = state
        .wiki
        .root()
        .join(ws.to_string())
        .join(proj.to_string())
        .join("notes")
        .join("doomed.md");
    assert!(file.exists(), "page file present before delete");

    let resp = post(
        state,
        "/admin/delete-page",
        json!({ "workspace": "default", "project": "scratch", "path": path }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // On-disk file removed (proves the delete reached the wiki).
    assert!(!file.exists(), "page file removed from disk: {file:?}");
}

#[tokio::test]
async fn delete_page_unknown_project_returns_404() {
    let tmp = TempDir::new().unwrap();
    let (state, _store) = make_state(&tmp).await;
    let resp = post(
        state,
        "/admin/delete-page",
        json!({ "workspace": "default", "project": "ghost", "path": "notes/x.md" }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
