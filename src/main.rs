mod auth;
mod categories;
mod items;
mod menu;
mod restaurants;
mod qr;
mod uploads;

use auth::AppState;
use axum::{
    extract::DefaultBodyLimit,
    routing::{get, post},
    Router,
};
use object_store::aws::AmazonS3Builder;
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_sessions::session::Expiry;
use tower_sessions::SessionManagerLayer;
use tower_sessions_sqlx_store::PostgresStore;

async fn health() -> &'static str {
    "ok"
}

fn validate_base_url(raw: &str) -> Result<String, String> {
    let parsed = url::Url::parse(raw)
        .map_err(|e| format!("BASE_URL is not a valid URL ({raw:?}): {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        s => return Err(format!("BASE_URL scheme must be http or https, got {s:?} in {raw:?}")),
    }
    if parsed.host().is_none() {
        return Err(format!("BASE_URL must have a non-empty host, got: {raw:?}"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!("BASE_URL must not contain credentials, got: {raw:?}"));
    }
    if parsed.query().is_some() {
        return Err(format!("BASE_URL must not contain a query string, got: {raw:?}"));
    }
    if parsed.fragment().is_some() {
        return Err(format!("BASE_URL must not contain a fragment, got: {raw:?}"));
    }
    let mut origin = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap());
    if let Some(port) = parsed.port() {
        origin.push(':');
        origin.push_str(&port.to_string());
    }
    Ok(origin)
}

struct EnvConfig {
    database_url: String,
    base_url: String,
    s3_endpoint: String,
    s3_bucket: String,
    aws_access_key_id: String,
    aws_secret_access_key: String,
    aws_region: String,
}

fn collect_env_config() -> Result<EnvConfig, Vec<String>> {
    let mut errors: Vec<String> = Vec::new();

    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| errors.push("DATABASE_URL must be set (e.g. postgres://user:pass@localhost/qr_menu_app)".to_string()))
        .ok();

    let base_url_raw = std::env::var("BASE_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string());
    let base_url = validate_base_url(&base_url_raw)
        .map_err(|e| errors.push(e))
        .ok();

    let s3_endpoint = std::env::var("S3_ENDPOINT")
        .map_err(|_| errors.push("S3_ENDPOINT must be set (e.g. http://localhost:9000 or https://s3.example.com)".to_string()))
        .ok();

    let s3_bucket = std::env::var("BUCKET")
        .map_err(|_| errors.push("BUCKET must be set".to_string()))
        .ok();

    let aws_access_key_id = std::env::var("AWS_ACCESS_KEY_ID")
        .map_err(|_| errors.push("AWS_ACCESS_KEY_ID must be set".to_string()))
        .ok();

    let aws_secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY")
        .map_err(|_| errors.push("AWS_SECRET_ACCESS_KEY must be set".to_string()))
        .ok();

    let aws_region = std::env::var("AWS_REGION")
        .unwrap_or_else(|_| "auto".to_string());

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(EnvConfig {
        database_url: database_url.unwrap(),
        base_url: base_url.unwrap(),
        s3_endpoint: s3_endpoint.unwrap(),
        s3_bucket: s3_bucket.unwrap(),
        aws_access_key_id: aws_access_key_id.unwrap(),
        aws_secret_access_key: aws_secret_access_key.unwrap(),
        aws_region,
    })
}

#[tokio::main]
async fn main() {
    let cfg = collect_env_config().unwrap_or_else(|errors| {
        panic!(
            "startup configuration errors ({} problem(s)):\n  - {}",
            errors.len(),
            errors.join("\n  - ")
        )
    });

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&cfg.database_url)
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
    let session_layer = SessionManagerLayer::new(session_store)
        .with_expiry(Expiry::OnInactivity(time::Duration::days(7)));

    // Allow plain HTTP only for endpoints that explicitly use http://.
    // This prevents credentials from being sent over plaintext if config drifts to HTTPS.
    let allow_http = cfg.s3_endpoint.starts_with("http://");

    // Canonical base for constructing public photo URLs: endpoint + bucket.
    let s3_public_base = format!(
        "{}/{}",
        cfg.s3_endpoint.trim_end_matches('/'),
        cfg.s3_bucket
    );

    let s3: Arc<dyn object_store::ObjectStore> = Arc::new(
        AmazonS3Builder::new()
            .with_endpoint(&cfg.s3_endpoint)
            .with_bucket_name(&cfg.s3_bucket)
            .with_access_key_id(&cfg.aws_access_key_id)
            .with_secret_access_key(&cfg.aws_secret_access_key)
            .with_region(&cfg.aws_region)
            .with_allow_http(allow_http)
            .build()
            .expect("failed to build S3 client"),
    );

    let state = AppState {
        pool,
        base_url: cfg.base_url,
        s3,
        s3_bucket: cfg.s3_bucket,
        s3_public_base,
        auth_rate_limiter: Arc::new(auth::AuthRateLimiter::new()),
    };

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
        .route(
            "/items/{id}/photo",
            post(uploads::upload_photo)
                .layer(DefaultBodyLimit::max(6 * 1024 * 1024)),
        )
        .route("/m/{slug}", get(menu::public_menu))
        .route("/restaurants/{id}/qr", get(qr::qr_svg))
        .layer(session_layer)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("failed to bind to 0.0.0.0:3000");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .expect("server error");
}
