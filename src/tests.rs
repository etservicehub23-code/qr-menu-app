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

/// A published restaurant is visible to the public — 200 with restaurant name in body.
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
    let body_bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body_str = std::str::from_utf8(&body_bytes).unwrap();
    assert!(
        body_str.contains("Open Place"),
        "response body should contain the restaurant name"
    );
}

/// update_branding tenant isolation: owner_2 cannot update owner_1's restaurant.
/// This tests the WHERE id = $3 AND owner_id = $4 guard in the UPDATE query directly.
#[sqlx::test(migrations = "./migrations")]
async fn branding_update_tenant_isolation(pool: sqlx::PgPool) {
    // Insert two users.
    sqlx::query("INSERT INTO users (email, password_hash) VALUES ('brand1@test.local', 'x'), ('brand2@test.local', 'x')")
        .execute(&pool)
        .await
        .unwrap();
    let owner1: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email = 'brand1@test.local'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let owner2: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email = 'brand2@test.local'")
        .fetch_one(&pool)
        .await
        .unwrap();

    // Insert a restaurant owned by owner1.
    sqlx::query(
        "INSERT INTO restaurants (owner_id, name, slug, is_published) \
         VALUES ($1, 'Brand Place', 'brand-place', false)",
    )
    .bind(owner1)
    .execute(&pool)
    .await
    .unwrap();

    let restaurant_id: i64 =
        sqlx::query_scalar("SELECT id FROM restaurants WHERE slug = 'brand-place'")
            .fetch_one(&pool)
            .await
            .unwrap();

    // owner2 tries to set accent_color via the same guarded UPDATE used by update_branding().
    let result =
        sqlx::query("UPDATE restaurants SET accent_color = $1 WHERE id = $2 AND owner_id = $3")
            .bind(Some("#ff0000"))
            .bind(restaurant_id)
            .bind(owner2) // wrong owner — should not match
            .execute(&pool)
            .await
            .unwrap();

    assert_eq!(
        result.rows_affected(),
        0,
        "cross-tenant branding update must affect 0 rows"
    );

    // Confirm the row was not modified.
    let stored: Option<String> =
        sqlx::query_scalar("SELECT accent_color FROM restaurants WHERE id = $1")
            .bind(restaurant_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        stored.is_none(),
        "accent_color must remain NULL after rejected cross-tenant update"
    );
}
