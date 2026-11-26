mod auth;
mod categories;
mod items;
mod menu;
mod qr;
mod restaurants;
#[cfg(test)]
mod tests;
mod uploads;

use auth::AppState;
use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};
use object_store::aws::AmazonS3Builder;
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_sessions::SessionManagerLayer;
use tower_sessions::session::Expiry;
use tower_sessions_sqlx_store::PostgresStore;

async fn health() -> &'static str {
    "ok"
}

fn validate_base_url(raw: &str) -> Result<String, String> {
    let parsed = url::Url::parse(raw).map_err(|e| format!("BASE_URL is not a valid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => {}
        _ => {
            return Err("BASE_URL scheme must be http or https".to_string());
        }
    }
    if parsed.host().is_none() {
        return Err("BASE_URL must have a non-empty host".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("BASE_URL must not contain credentials".to_string());
    }
    if parsed.query().is_some() {
        return Err("BASE_URL must not contain a query string".to_string());
    }
    if parsed.fragment().is_some() {
        return Err("BASE_URL must not contain a fragment".to_string());
    }
    let mut origin = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap());
    if let Some(port) = parsed.port() {
        origin.push(':');
        origin.push_str(&port.to_string());
    }
    Ok(origin)
}
/// Validate S3_PUBLIC_BASE_URL: must be an absolute http/https URL with a non-empty host,
/// no credentials, no query string, and no fragment. Path is allowed (may include bucket).
/// Trailing slash is stripped for consistent prefix construction. Empty string is rejected.
/// Syntax violations (parser-repaired inputs like `https:///bucket`) are rejected.
/// Error messages never echo the raw value to avoid leaking credentials.
fn validate_s3_public_base_url(raw: &str) -> Result<String, String> {
    if raw.trim().is_empty() {
        return Err(
            "S3_PUBLIC_BASE_URL must not be empty; leave it unset to use the default".to_string(),
        );
    }
    // Reject parser-repaired inputs (e.g. https:///bucket accepted as host="bucket",
    // embedded newlines in hostname silently stripped, etc.).
    // syntax_violation_callback takes Fn (not FnMut), so RefCell for interior mutability.
    let violations: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let parsed = url::Url::options()
        .syntax_violation_callback(Some(&|v: url::SyntaxViolation| {
            violations.borrow_mut().push(v.description().to_string());
        }))
        .parse(raw)
        .map_err(|e| format!("S3_PUBLIC_BASE_URL is not a valid URL: {e}"))?;
    let violations = violations.into_inner();
    if !violations.is_empty() {
        return Err(format!(
            "S3_PUBLIC_BASE_URL contains syntax violations: {}",
            violations.join(", ")
        ));
    }
    match parsed.scheme() {
        "http" | "https" => {}
        _ => {
            // Do not echo scheme — malformed input could place secrets there.
            return Err("S3_PUBLIC_BASE_URL scheme must be http or https".to_string());
        }
    }
    if parsed.host().is_none() {
        return Err("S3_PUBLIC_BASE_URL must have a non-empty host".to_string());
    }
    // Do not echo raw value in credential/query/fragment errors — the raw string
    // may contain secrets (password, token in query param).
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("S3_PUBLIC_BASE_URL must not contain credentials".to_string());
    }
    if parsed.query().is_some() {
        return Err("S3_PUBLIC_BASE_URL must not contain a query string".to_string());
    }
    if parsed.fragment().is_some() {
        return Err("S3_PUBLIC_BASE_URL must not contain a fragment".to_string());
    }
    // Use the parser's canonical serialization (trailing slash already stripped by
    // our empty-host/query/fragment guards). Strip any remaining trailing slash.
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

struct EnvConfig {
    database_url: String,
    base_url: String,
    s3_endpoint: String,
    s3_bucket: String,
    /// Optional override for the public base URL used in photo image URLs.
    /// Defaults to `{s3_endpoint}/{s3_bucket}` when not set.
    /// Set this when the internal S3 endpoint differs from the browser-accessible URL
    /// (e.g. CDN, reverse proxy, or Docker-internal hostname vs. localhost).
    s3_public_base_url: Option<String>,
    aws_access_key_id: String,
    aws_secret_access_key: String,
    aws_region: String,
}

fn collect_env_config() -> Result<EnvConfig, Vec<String>> {
    let mut errors: Vec<String> = Vec::new();

    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| {
            errors.push(
                "DATABASE_URL must be set (e.g. postgres://user:pass@localhost/qr_menu_app)"
                    .to_string(),
            )
        })
        .ok();

    let base_url_raw =
        std::env::var("BASE_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let base_url = validate_base_url(&base_url_raw)
        .map_err(|e| errors.push(e))
        .ok();

    let s3_endpoint = std::env::var("S3_ENDPOINT")
        .map_err(|_| {
            errors.push(
                "S3_ENDPOINT must be set (e.g. http://localhost:9000 or https://s3.example.com)"
                    .to_string(),
            )
        })
        .ok();

    let s3_bucket = std::env::var("BUCKET")
        .map_err(|_| errors.push("BUCKET must be set".to_string()))
        .ok();

    let s3_public_base_url = match std::env::var("S3_PUBLIC_BASE_URL") {
        Ok(raw) => validate_s3_public_base_url(&raw)
            .map_err(|e| errors.push(e))
            .ok(),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            errors.push("S3_PUBLIC_BASE_URL contains invalid Unicode".to_string());
            None
        }
    };

    let aws_access_key_id = std::env::var("AWS_ACCESS_KEY_ID")
        .map_err(|_| errors.push("AWS_ACCESS_KEY_ID must be set".to_string()))
        .ok();

    let aws_secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY")
        .map_err(|_| errors.push("AWS_SECRET_ACCESS_KEY must be set".to_string()))
        .ok();

    let aws_region = std::env::var("AWS_REGION").unwrap_or_else(|_| "auto".to_string());

    if !errors.is_empty() {
        return Err(errors);
    }

    Ok(EnvConfig {
        database_url: database_url.unwrap(),
        base_url: base_url.unwrap(),
        s3_endpoint: s3_endpoint.unwrap(),
        s3_bucket: s3_bucket.unwrap(),
        s3_public_base_url,
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

    // Canonical base for constructing public photo URLs.
    // Use S3_PUBLIC_BASE_URL when set (e.g. CDN or browser-accessible hostname);
    // fall back to "{s3_endpoint}/{s3_bucket}" for local/default deployments.
    let s3_public_base = match cfg.s3_public_base_url {
        Some(explicit) => explicit.trim_end_matches('/').to_string(),
        None => format!(
            "{}/{}",
            cfg.s3_endpoint.trim_end_matches('/'),
            cfg.s3_bucket
        ),
    };

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
        s3_public_base,
        auth_rate_limiter: Arc::new(auth::AuthRateLimiter::new()),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/", get(auth::index))
        .route("/signup", get(auth::signup_form).post(auth::signup))
        .route("/login", get(auth::login_form).post(auth::login))
        .route("/logout", post(auth::logout))
        .route(
            "/restaurants/new",
            get(restaurants::new_form).post(restaurants::create),
        )
        .route("/restaurants/{id}", get(restaurants::show))
        .route(
            "/restaurants/{id}/publish",
            post(restaurants::publish_toggle),
        )
        .route(
            "/restaurants/{id}/branding",
            post(restaurants::update_branding),
        )
        .route("/restaurants/{id}/categories", get(categories::list))
        .route(
            "/restaurants/{id}/categories/new",
            get(categories::new_form).post(categories::create),
        )
        .route(
            "/categories/{id}/edit",
            get(categories::edit_form).post(categories::edit),
        )
        .route("/categories/{id}/delete", post(categories::delete))
        .route("/categories/{id}/items", get(items::list))
        .route(
            "/categories/{id}/items/new",
            get(items::new_form).post(items::create),
        )
        .route("/items/{id}/edit", get(items::edit_form).post(items::edit))
        .route("/items/{id}/delete", post(items::delete))
        .route("/items/{id}/toggle", post(items::toggle))
        .route(
            "/items/{id}/photo",
            post(uploads::upload_photo).layer(DefaultBodyLimit::max(6 * 1024 * 1024)),
        )
        .route("/m/{slug}", get(menu::public_menu))
        .route("/restaurants/{id}/qr", get(qr::qr_svg))
        .layer(session_layer)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("failed to bind to 0.0.0.0:3000");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("server error");
}

#[cfg(test)]
mod validator_tests {
    use super::{validate_base_url, validate_s3_public_base_url};

    /// Table-driven tests covering rejection and valid canonical output.
    #[test]
    fn valid_inputs_produce_canonical_output() {
        let cases: &[(&str, &str)] = &[
            (
                "http://localhost:9000/bucket",
                "http://localhost:9000/bucket",
            ),
            (
                "https://cdn.example.com/photos",
                "https://cdn.example.com/photos",
            ),
            // Trailing slash stripped.
            (
                "http://localhost:9000/bucket/prefix/",
                "http://localhost:9000/bucket/prefix",
            ),
            // Root path with trailing slash stripped.
            ("https://cdn.example.com/", "https://cdn.example.com"),
            ("https://cdn.example.com", "https://cdn.example.com"),
            // IPv6 host.
            ("http://[::1]:9000/bucket", "http://[::1]:9000/bucket"),
            // Non-default port.
            (
                "http://192.168.1.1:9000/bucket",
                "http://192.168.1.1:9000/bucket",
            ),
            // Hostname case-normalised + default HTTPS port stripped by url crate.
            // A "just trim trailing slash" implementation would return the raw input
            // unchanged, so this row catches that regression.
            (
                "https://CDN.Example.COM:443/photos/",
                "https://cdn.example.com/photos",
            ),
        ];
        for (input, expected) in cases {
            let result = validate_s3_public_base_url(input);
            assert!(
                result.is_ok(),
                "expected Ok for {input:?}, got Err: {:?}",
                result.err()
            );
            assert_eq!(
                result.unwrap().as_str(),
                *expected,
                "canonical mismatch for {input:?}"
            );
        }
    }

    #[test]
    fn empty_and_whitespace_rejected() {
        for input in ["", "   ", "\t"] {
            assert!(
                validate_s3_public_base_url(input).is_err(),
                "expected Err for {input:?}"
            );
        }
    }

    /// https:///bucket triggers a syntax violation (ExpectedDoubleSlash) in the url crate.
    #[test]
    fn syntax_violation_rejected() {
        // Empty authority after double slash.
        assert!(validate_s3_public_base_url("https:///bucket").is_err());
        // Embedded newline in hostname triggers TabOrNewlineIgnored violation.
        assert!(validate_s3_public_base_url("https://exam\nple.com/bucket").is_err());
    }

    #[test]
    fn bad_scheme_rejected() {
        for input in ["ftp://example.com/bucket", "file:///etc/passwd"] {
            assert!(
                validate_s3_public_base_url(input).is_err(),
                "expected Err for {input:?}"
            );
        }
    }

    #[test]
    fn credentials_rejected_no_secret_in_error() {
        let secret = "TOPSECRETTOKEN";
        let cases = [
            format!("https://user:{secret}@cdn.example.com/bucket"),
            format!("https://{secret}@cdn.example.com/bucket"), // username = secret
        ];
        for case in &cases {
            let result = validate_s3_public_base_url(case);
            assert!(result.is_err(), "expected Err for {case:?}");
            let err = result.unwrap_err();
            assert!(
                !err.contains(secret),
                "error message for credentials case leaked secret: {err:?}"
            );
        }
    }

    #[test]
    fn query_string_rejected_no_secret_in_error() {
        let secret = "TOPSECRETTOKEN";
        let case = format!("https://cdn.example.com/bucket?token={secret}");
        let result = validate_s3_public_base_url(&case);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            !err.contains(secret),
            "error message for query case leaked secret: {err:?}"
        );
    }

    #[test]
    fn fragment_rejected_no_secret_in_error() {
        let secret = "FRAGMENTSECRET";
        let case = format!("https://cdn.example.com/bucket#{secret}");
        let result = validate_s3_public_base_url(&case);
        assert!(result.is_err(), "expected fragment to be rejected");
        let err = result.unwrap_err();
        assert!(
            !err.contains(secret),
            "fragment error leaked secret: {err:?}"
        );
    }

    #[test]
    fn scheme_error_no_secret_in_error() {
        // A secret pasted before :// occupies the scheme field — verify no echo.
        let secret = "SCHEMESECRET";
        // url crate lowercases the scheme; "schemesecret" is a valid scheme token.
        let case = format!("{secret}://cdn.example.com/bucket");
        let result = validate_s3_public_base_url(&case);
        assert!(
            result.is_err(),
            "expected unsupported scheme to be rejected"
        );
        let err = result.unwrap_err();
        assert!(
            !err.to_lowercase().contains(&secret.to_lowercase()),
            "scheme error leaked secret: {err:?}"
        );
    }

    #[test]
    fn username_only_credential_rejected_no_secret_in_error() {
        // username-only: no password, but still a credential that must be rejected.
        let secret = "USERNAMESECRET";
        let case = format!("https://{secret}@cdn.example.com/bucket");
        let result = validate_s3_public_base_url(&case);
        assert!(
            result.is_err(),
            "expected username-only credential to be rejected"
        );
        let err = result.unwrap_err();
        assert!(
            !err.contains(secret),
            "credential error leaked username: {err:?}"
        );
    }

    /// Verify that construction + strip_prefix roundtrip recovers the original object key.
    /// This is the invariant uploads.rs and items.rs depend on.
    /// Also asserts the full stored URL to catch doubled-separator or trailing-slash bugs.
    #[test]
    fn strip_prefix_roundtrip() {
        let cases: &[(&str, &str)] = &[
            (
                "http://localhost:9000/my-bucket",
                "http://localhost:9000/my-bucket/menu-items/42/abc123.jpg",
            ),
            (
                "https://cdn.example.com/photos",
                "https://cdn.example.com/photos/menu-items/42/abc123.jpg",
            ),
            (
                "https://cdn.example.com",
                "https://cdn.example.com/menu-items/42/abc123.jpg",
            ),
        ];
        let object_key = "menu-items/42/abc123.jpg";
        for (base_raw, expected_stored_url) in cases {
            let canonical = validate_s3_public_base_url(base_raw).unwrap();
            let stored_url = format!("{canonical}/{object_key}");
            // Assert the complete stored URL first (catches doubled-separator or slash bugs).
            assert_eq!(
                stored_url.as_str(),
                *expected_stored_url,
                "stored URL mismatch for base {base_raw:?}"
            );
            // Then verify strip_prefix recovery.
            let recovered = stored_url
                .strip_prefix(&format!("{canonical}/"))
                .unwrap_or_else(|| panic!("strip_prefix failed for base {base_raw:?}"));
            assert_eq!(recovered, object_key, "roundtrip mismatch for {base_raw:?}");
        }
    }

    // ---- validate_base_url tests ----
    // These tests guard against future reintroduction of raw-value echoing in
    // validate_base_url() error messages (oracle concern from commit d7949dc).

    #[test]
    fn base_url_valid_inputs_produce_canonical_origin() {
        let cases: &[(&str, &str)] = &[
            ("http://localhost:3000", "http://localhost:3000"),
            ("https://example.com", "https://example.com"),
            // Port preserved when non-default.
            ("http://example.com:8080", "http://example.com:8080"),
            // Default HTTPS port stripped by url crate; path silently dropped from origin.
            ("https://example.com:443/some/path", "https://example.com"),
            // Hostname case-normalised.
            ("https://EXAMPLE.COM", "https://example.com"),
            // IPv6 host.
            ("http://[::1]:3000", "http://[::1]:3000"),
        ];
        for (input, expected) in cases {
            let result = validate_base_url(input);
            assert!(
                result.is_ok(),
                "expected Ok for {input:?}, got Err: {:?}",
                result.err()
            );
            assert_eq!(
                result.unwrap().as_str(),
                *expected,
                "canonical mismatch for {input:?}"
            );
        }
    }

    #[test]
    fn base_url_credentials_rejected_no_secret_in_error() {
        let secret = "BASEURLSECRET";
        let cases = [
            format!("https://user:{secret}@example.com"),
            format!("https://{secret}@example.com"), // username = secret
        ];
        for case in &cases {
            let result = validate_base_url(case);
            assert!(result.is_err(), "expected Err for credential case {case:?}");
            let err = result.unwrap_err();
            assert!(
                !err.contains(secret),
                "error message leaked secret for {case:?}: {err:?}"
            );
        }
    }

    #[test]
    fn base_url_query_rejected_no_secret_in_error() {
        let secret = "BASEURLQUERYSECRET";
        let case = format!("https://example.com?token={secret}");
        let result = validate_base_url(&case);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(!err.contains(secret), "query error leaked secret: {err:?}");
    }

    #[test]
    fn base_url_fragment_rejected_no_secret_in_error() {
        let secret = "BASEURLFRAGSECRET";
        let case = format!("https://example.com#{secret}");
        let result = validate_base_url(&case);
        assert!(result.is_err(), "expected fragment to be rejected");
        let err = result.unwrap_err();
        assert!(
            !err.contains(secret),
            "fragment error leaked secret: {err:?}"
        );
    }

    #[test]
    fn base_url_bad_scheme_rejected() {
        for input in ["ftp://example.com", "file:///etc/passwd"] {
            assert!(
                validate_base_url(input).is_err(),
                "expected Err for bad scheme {input:?}"
            );
        }
        // Unsupported scheme with embedded credentials — secret must not appear in error.
        let secret = "BASEURLSCHSECRET";
        let case = format!("ftp://user:{secret}@example.com");
        let result = validate_base_url(&case);
        assert!(
            result.is_err(),
            "expected Err for bad scheme with credentials"
        );
        let err = result.unwrap_err();
        assert!(
            !err.contains(secret),
            "bad-scheme error leaked secret: {err:?}"
        );
    }

    #[test]
    fn base_url_invalid_rejected() {
        assert!(validate_base_url("not-a-url").is_err());
        assert!(validate_base_url("").is_err());
        // Parse failure with embedded credentials — secret must not appear in error.
        let secret = "BASEURLPARSESECRET";
        let case = format!("https://user:{secret}@example.com:invalid");
        let result = validate_base_url(&case);
        assert!(
            result.is_err(),
            "expected Err for malformed URL with credentials"
        );
        let err = result.unwrap_err();
        assert!(
            !err.contains(secret),
            "parse-error message leaked secret: {err:?}"
        );
    }
}
