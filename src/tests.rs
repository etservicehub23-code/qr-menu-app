use crate::auth::AppState;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use sqlx::PgPool;
use std::sync::Arc;
use tower::ServiceExt;
use tower_sessions::SessionManagerLayer;
use tower_sessions_sqlx_store::PostgresStore;

/// Build a minimal router containing only the public menu route,
/// backed by a real Postgres pool (provided by sqlx::test) and an
/// in-memory object store (S3 not needed for the public menu path).
async fn menu_test_app(pool: PgPool) -> Router {
    let session_store = PostgresStore::new(pool.clone());
    session_store.migrate().await.unwrap();
    let session_layer = SessionManagerLayer::new(session_store);
    let s3: Arc<dyn object_store::ObjectStore> = Arc::new(object_store::memory::InMemory::new());
    let state = AppState {
        pool,
        base_url: "http://localhost:3000".to_string(),
        s3,
        s3_public_base: "http://localhost:9000/test".to_string(),
        auth_rate_limiter: Arc::new(crate::auth::AuthRateLimiter::new()),
    };
    Router::new()
        .route("/m/{slug}", get(crate::menu::public_menu))
        .layer(session_layer)
        .with_state(state)
}

/// A slug that was never inserted returns 404.
#[sqlx::test(migrations = "./migrations")]
async fn menu_nonexistent_slug_returns_404(pool: PgPool) {
    let app = menu_test_app(pool).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/m/no-such-restaurant")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// An existing but unpublished restaurant is hidden from the public — 404.
#[sqlx::test(migrations = "./migrations")]
async fn menu_unpublished_restaurant_returns_404(pool: PgPool) {
    sqlx::query("INSERT INTO users (email, password_hash) VALUES ('owner@test.local', 'x')")
        .execute(&pool)
        .await
        .unwrap();
    let owner_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email = 'owner@test.local'")
        .fetch_one(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO restaurants (owner_id, name, slug, is_published) \
         VALUES ($1, 'Draft Place', 'draft-place', false)",
    )
    .bind(owner_id)
    .execute(&pool)
    .await
    .unwrap();

    let app = menu_test_app(pool).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/m/draft-place")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// A published restaurant is visible to the public — 200.
#[sqlx::test(migrations = "./migrations")]
async fn menu_published_restaurant_returns_200(pool: PgPool) {
    sqlx::query("INSERT INTO users (email, password_hash) VALUES ('owner2@test.local', 'x')")
        .execute(&pool)
        .await
        .unwrap();
    let owner_id: i64 =
        sqlx::query_scalar("SELECT id FROM users WHERE email = 'owner2@test.local'")
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO restaurants (owner_id, name, slug, is_published) \
         VALUES ($1, 'Open Place', 'open-place', true)",
    )
    .bind(owner_id)
    .execute(&pool)
    .await
    .unwrap();

    let app = menu_test_app(pool).await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/m/open-place")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
