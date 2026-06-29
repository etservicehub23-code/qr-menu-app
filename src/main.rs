mod auth;
mod escape;
mod categories;
mod items;
mod menu;
mod restaurants;
mod qr;

use auth::AppState;
use axum::{
    routing::{get, post},
    Router,
};
use sqlx::postgres::PgPoolOptions;
use tower_sessions::SessionManagerLayer;
use tower_sessions_sqlx_store::PostgresStore;

async fn health() -> &'static str {
    "ok"
}

fn validate_base_url(raw: &str) -> String {
    let parsed = url::Url::parse(raw)
        .unwrap_or_else(|e| panic!("BASE_URL is not a valid URL ({raw:?}): {e}"));
    match parsed.scheme() {
        "http" | "https" => {}
        s => panic!("BASE_URL scheme must be http or https, got {s:?} in {raw:?}"),
    }
    if parsed.host().is_none() {
        panic!("BASE_URL must have a non-empty host, got: {raw:?}");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        panic!("BASE_URL must not contain credentials, got: {raw:?}");
    }
    if parsed.query().is_some() {
        panic!("BASE_URL must not contain a query string, got: {raw:?}");
    }
    if parsed.fragment().is_some() {
        panic!("BASE_URL must not contain a fragment, got: {raw:?}");
    }
    // Build canonical origin: scheme + host + optional non-default port
    let mut origin = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap());
    if let Some(port) = parsed.port() {
        origin.push(':');
        origin.push_str(&port.to_string());
    }
    origin
}

#[tokio::main]
async fn main() {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set (e.g. postgres://user:pass@localhost/qr_menu_app)");

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("failed to connect to Postgres");

    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("failed to run database migrations");

    let session_store = PostgresStore::new(pool.clone());
    session_store
        .migrate()
        .await
        .expect("failed to run session store migrations");
    let session_layer = SessionManagerLayer::new(session_store);

    let base_url_raw = std::env::var("BASE_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string());
    let base_url = validate_base_url(&base_url_raw);

    let state = AppState { pool, base_url };

    let app = Router::new()
        .route("/health", get(health))
        .route("/", get(auth::index))
        .route("/signup", get(auth::signup_form).post(auth::signup))
        .route("/login", get(auth::login_form).post(auth::login))
        .route("/logout", post(auth::logout))
        .route("/restaurants/new", get(restaurants::new_form).post(restaurants::create))
        .route("/restaurants/{id}", get(restaurants::show))
        .route("/restaurants/{id}/publish", post(restaurants::publish_toggle))
        .route("/restaurants/{id}/categories", get(categories::list))
        .route("/restaurants/{id}/categories/new", get(categories::new_form).post(categories::create))
        .route("/categories/{id}/items", get(items::list))
        .route("/categories/{id}/items/new", get(items::new_form).post(items::create))
        .route("/items/{id}/edit", get(items::edit_form).post(items::edit))
        .route("/items/{id}/delete", post(items::delete))
        .route("/items/{id}/toggle", post(items::toggle))
        .route("/m/{slug}", get(menu::public_menu))
        .route("/restaurants/{id}/qr", get(qr::qr_svg))
        .layer(session_layer)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("failed to bind to 0.0.0.0:3000");
    axum::serve(listener, app)
        .await
        .expect("server error");
}
