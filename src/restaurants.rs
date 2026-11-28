use crate::auth::{AppState, USER_ID_KEY, new_csrf_token, verify_csrf_token};
use askama::Template;
use axum::extract::{Form, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use serde::Deserialize;
use tower_sessions::Session;
use url::Url;

#[derive(Template)]
#[template(path = "restaurant_new.html")]
struct RestaurantNewPage {
    token: String,
}

#[derive(Template)]
#[template(path = "restaurant_show.html")]
struct RestaurantShowPage {
    id: i64,
    name: String,
    slug: String,
    is_published: bool,
    token: String,
    branding_token: String,
    scan_count: i64,
    accent_color: Option<String>,
    logo_url: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateRestaurantForm {
    name: String,
    authenticity_token: String,
}

#[derive(Deserialize)]
pub struct TokenForm {
    authenticity_token: String,
}

#[derive(Deserialize)]
pub struct BrandingForm {
    authenticity_token: String,
    accent_color: String,
    logo_url: String,
}

/// Converts a restaurant name to a URL-safe slug:
/// lowercase, spaces → hyphens, strip non-alphanumeric/hyphen, collapse
/// repeated hyphens, trim leading/trailing hyphens.
fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Finds a unique slug by appending -2, -3, … if the base slug is taken.
async fn unique_slug(
    pool: &sqlx::PgPool,
    base: &str,
) -> Result<String, (StatusCode, &'static str)> {
    let base = base.get(..61).unwrap_or(base); // 61 + len("-99")=3 <= 64-char DB max
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM restaurants WHERE slug = $1)")
            .bind(base)
            .fetch_one(pool)
            .await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    if !exists {
        return Ok(base.to_string());
    }

    for n in 2u32..=99 {
        let candidate = format!("{base}-{n}");
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM restaurants WHERE slug = $1)")
                .bind(&candidate)
                .fetch_one(pool)
                .await
                .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
        if !exists {
            return Ok(candidate);
        }
    }

    Err((
        StatusCode::CONFLICT,
        "could not generate a unique slug for this name",
    ))
}

pub async fn new_form(session: Session) -> Result<Html<String>, (StatusCode, &'static str)> {
    let _user_id = require_auth(&session).await?;
    let token = new_csrf_token(&session).await?;
    let html = RestaurantNewPage { token }
        .render()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "template error"))?;
    Ok(Html(html))
}

pub async fn create(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<CreateRestaurantForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    verify_csrf_token(&session, &form.authenticity_token).await?;

    let name = form.name.trim();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "restaurant name must not be empty"));
    }
    if name.len() > 120 {
        return Err((
            StatusCode::BAD_REQUEST,
            "restaurant name must be 120 characters or fewer",
        ));
    }

    let base_slug = slugify(name);
    if base_slug.len() < 3 {
        return Err((
            StatusCode::BAD_REQUEST,
            "restaurant name must produce a valid URL slug (try adding more letters)",
        ));
    }
    let slug = unique_slug(&state.pool, &base_slug).await?;

    let restaurant_id: i64 = sqlx::query_scalar(
        "INSERT INTO restaurants (owner_id, name, slug) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(user_id)
    .bind(name)
    .bind(&slug)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to create restaurant",
        )
    })?;

    Ok(Redirect::to(&format!("/restaurants/{restaurant_id}")))
}

pub async fn show(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;

    type ShowRow = (String, String, bool, Option<String>, Option<String>);
    let row: Option<ShowRow> = sqlx::query_as(
        "SELECT name, slug, is_published, accent_color, logo_url
         FROM restaurants WHERE id = $1 AND owner_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    let Some((name, slug, is_published, accent_color, logo_url)) = row else {
        return Err((StatusCode::NOT_FOUND, "restaurant not found"));
    };

    let scan_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM menu_scans WHERE restaurant_id = $1")
            .bind(id)
            .fetch_one(&state.pool)
            .await
            .unwrap_or(0);

    let token = new_csrf_token(&session).await?;
    let branding_token = new_csrf_token(&session).await?;
    let html = RestaurantShowPage {
        id,
        name,
        slug,
        is_published,
        token,
        branding_token,
        scan_count,
        accent_color,
        logo_url,
    }
    .render()
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "template error"))?;
    Ok(Html(html))
}

pub async fn publish_toggle(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Form(form): Form<TokenForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    verify_csrf_token(&session, &form.authenticity_token).await?;

    let rows = sqlx::query(
        "UPDATE restaurants SET is_published = NOT is_published
         WHERE id = $1 AND owner_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .execute(&state.pool)
    .await
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to toggle publish status",
        )
    })?;

    if rows.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "restaurant not found"));
    }

    Ok(Redirect::to(&format!("/restaurants/{id}")))
}

/// Updates accent_color and logo_url for the authenticated owner's restaurant.
/// accent_color must be empty (clear) or a hex color (#RGB or #RRGGBB).
/// logo_url must be empty (clear) or an absolute http/https URL.
/// Query strings are permitted so CDN signed/transformation URLs work.
/// Fragments are rejected (meaningless in an img src and never sent to the server).
pub async fn update_branding(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Path(id): axum::extract::Path<i64>,
    Form(form): Form<BrandingForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    verify_csrf_token(&session, &form.authenticity_token).await?;

    let accent = form.accent_color.trim();
    let logo = form.logo_url.trim();

    // Validate accent_color: must be empty or #RGB / #RRGGBB hex.
    let accent_color: Option<&str> = if accent.is_empty() {
        None
    } else if is_valid_hex_color(accent) {
        Some(accent)
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            "accent color must be a hex color (#RGB or #RRGGBB)",
        ));
    };

    let logo_url: Option<String> =
        validate_logo_url(logo).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let rows = apply_branding_update(&state.pool, id, user_id, accent_color, logo_url.as_deref())
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to update branding",
            )
        })?;

    if rows.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "restaurant not found"));
    }

    Ok(Redirect::to(&format!("/restaurants/{id}")))
}

/// Executes the guarded branding UPDATE; returns the query result.
/// Shared by the handler and tests so they exercise identical SQL.
async fn apply_branding_update(
    pool: &sqlx::PgPool,
    restaurant_id: i64,
    owner_id: i64,
    accent_color: Option<&str>,
    logo_url: Option<&str>,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query(
        "UPDATE restaurants SET accent_color = $1, logo_url = $2
         WHERE id = $3 AND owner_id = $4",
    )
    .bind(accent_color)
    .bind(logo_url)
    .bind(restaurant_id)
    .bind(owner_id)
    .execute(pool)
    .await
}

/// Returns true if s is a valid CSS hex color: #RGB or #RRGGBB (case-insensitive).
fn is_valid_hex_color(s: &str) -> bool {
    let bytes = s.as_bytes();
    match bytes.len() {
        4 => bytes[0] == b'#' && bytes[1..].iter().all(|b| b.is_ascii_hexdigit()),
        7 => bytes[0] == b'#' && bytes[1..].iter().all(|b| b.is_ascii_hexdigit()),
        _ => false,
    }
}

/// Validates a logo URL input: empty string → None, valid http/https URL → Some(canonical).
/// Query strings are permitted (CDN signed/transformation URLs). Fragments are rejected.
fn validate_logo_url(raw: &str) -> Result<Option<String>, &'static str> {
    if raw.is_empty() {
        return Ok(None);
    }
    match Url::parse(raw) {
        Ok(parsed)
            if (parsed.scheme() == "http" || parsed.scheme() == "https")
                && parsed.host().is_some()
                && parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.fragment().is_none() =>
        {
            let canonical = parsed.to_string();
            if canonical.len() > 2048 {
                return Err("logo URL is too long");
            }
            Ok(Some(canonical))
        }
        _ => Err("logo URL must be a valid http or https URL"),
    }
}

/// Extracts the authenticated user_id from the session, or returns 401.
pub async fn require_auth(session: &Session) -> Result<i64, (StatusCode, &'static str)> {
    session
        .get::<i64>(USER_ID_KEY)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "session error"))?
        .ok_or((StatusCode::UNAUTHORIZED, "you must be logged in"))
}

#[cfg(test)]
mod tests {
    use super::{apply_branding_update, is_valid_hex_color, validate_logo_url};
    use sqlx::PgPool;

    // ---- is_valid_hex_color unit tests ----

    #[test]
    fn hex_color_valid() {
        assert!(is_valid_hex_color("#abc"));
        assert!(is_valid_hex_color("#ABC"));
        assert!(is_valid_hex_color("#aabbcc"));
        assert!(is_valid_hex_color("#AABBCC"));
        assert!(is_valid_hex_color("#1a2b3c"));
        assert!(is_valid_hex_color("#fff"));
        assert!(is_valid_hex_color("#000000"));
    }

    #[test]
    fn hex_color_invalid() {
        // Wrong length (can't pass on character check alone)
        assert!(!is_valid_hex_color(""));
        assert!(!is_valid_hex_color("#ab")); // 2 hex chars
        assert!(!is_valid_hex_color("#abcde")); // 5 hex chars
        assert!(!is_valid_hex_color("#aabbccdd")); // 8-digit not accepted

        // Correct length but no leading '#' — isolates the prefix check
        assert!(!is_valid_hex_color("xabc")); // length 4, wrong prefix
        assert!(!is_valid_hex_color("xaabbcc")); // length 7, wrong prefix

        // Correct length and '#' prefix but bad characters — isolates the char check
        assert!(!is_valid_hex_color("#abg")); // length 4, 'g' is not hex
        assert!(!is_valid_hex_color("#aabbcg")); // length 7, 'g' is not hex
        assert!(!is_valid_hex_color("#GGG")); // length 4, 'G' is not hex
        assert!(!is_valid_hex_color("#GGGGGG")); // length 7, 'G' is not hex

        // Edge cases
        assert!(!is_valid_hex_color("abc")); // no prefix, wrong length
        assert!(!is_valid_hex_color("# abc")); // length 6 — rejected by length check
        assert!(!is_valid_hex_color("##aabbcc")); // length 8 — rejected by length check
    }

    // ---- validate_logo_url unit tests ----

    #[test]
    fn logo_url_empty_returns_none() {
        assert_eq!(validate_logo_url(""), Ok(None));
    }

    #[test]
    fn logo_url_valid_no_query() {
        let result = validate_logo_url("https://cdn.example.com/logo.png");
        assert_eq!(
            result,
            Ok(Some("https://cdn.example.com/logo.png".to_string()))
        );
    }

    #[test]
    fn logo_url_valid_with_query_string() {
        // CDN signed URL — multiple params including percent-encoded chars must be
        // preserved exactly (not dropped or reordered) in the stored canonical form.
        let url = "https://cdn.example.com/logo.jpg?sig=abc%2B1&expires=9999999999&dup=x&dup=y&plus=%2B&amp=%26";
        let result = validate_logo_url(url);
        assert!(
            result.is_ok(),
            "query string should be allowed: {:?}",
            result
        );
        let stored = result.unwrap().unwrap();
        // All parameters, including percent-encoded chars, must be preserved verbatim.
        assert!(
            stored.contains("sig=abc%2B1"),
            "percent-encoded + preserved: {stored}"
        );
        assert!(
            stored.contains("dup=x&dup=y"),
            "duplicate params preserved: {stored}"
        );
        assert!(
            stored.contains("plus=%2B"),
            "percent-encoded & preserved: {stored}"
        );
        assert!(
            stored.contains("amp=%26"),
            "percent-encoded & preserved: {stored}"
        );
        // Exact round-trip: the canonical form must equal the input since there is
        // nothing to normalize in this URL.
        assert_eq!(stored, url, "exact URL round-trip must hold");
    }

    #[test]
    fn logo_url_length_boundary() {
        // A URL whose canonical form is exactly 2048 bytes must be accepted.
        // 36 bytes of fixed prefix: "https://cdn.example.com/logo.png?x="
        let prefix = "https://cdn.example.com/logo.png?x=";
        let padding = "a".repeat(2048 - prefix.len());
        let url_2048 = format!("{prefix}{padding}");
        assert_eq!(url_2048.len(), 2048);
        let result = validate_logo_url(&url_2048);
        assert_eq!(
            result,
            Ok(Some(url_2048.clone())),
            "2048-byte URL must be stored exactly"
        );

        // A URL whose canonical form is 2049 bytes must be rejected.
        let url_2049 = format!("{prefix}{}", "a".repeat(2049 - prefix.len()));
        assert_eq!(url_2049.len(), 2049);
        let result = validate_logo_url(&url_2049);
        assert!(result.is_err(), "2049-byte URL must be rejected");
    }

    #[test]
    fn logo_url_percent_encoding_expansion_rejected() {
        // A URL whose raw byte length is under 2048 but whose canonical form
        // (after percent-encoding non-ASCII chars) exceeds 2048 must be rejected.
        // é (U+00E9) is 2 UTF-8 bytes raw but encodes to %C3%A9 (6 bytes).
        // 400 × é = 800 raw bytes; canonical = 400 × 6 = 2400 bytes for the value.
        // Total: 36-byte prefix + 2400 = 2436 bytes canonical > 2048.
        let raw_url = format!("https://cdn.example.com/logo.png?x={}", "é".repeat(400));
        assert!(
            raw_url.len() < 2048,
            "fixture must have raw length < 2048 (got {})",
            raw_url.len()
        );
        let result = validate_logo_url(&raw_url);
        assert!(
            result.is_err(),
            "URL whose canonical form exceeds 2048 bytes must be rejected"
        );
    }

    #[test]
    fn logo_url_username_only_rejected() {
        // Username without password must also be rejected.
        let result = validate_logo_url("https://user@cdn.example.com/logo.png");
        assert!(result.is_err(), "username-only credential must be rejected");
    }

    #[test]
    fn logo_url_fragment_rejected() {
        let result = validate_logo_url("https://cdn.example.com/logo.png#section");
        assert!(result.is_err(), "fragment must be rejected");
    }

    #[test]
    fn logo_url_credentials_rejected() {
        let result = validate_logo_url("https://user:LOGOSECRET@cdn.example.com/logo.png");
        assert!(result.is_err());
        // Error must not echo the credential.
        let err = result.unwrap_err();
        assert!(
            !err.contains("LOGOSECRET"),
            "error must not expose the credential"
        );
    }

    #[test]
    fn logo_url_non_http_scheme_rejected() {
        assert!(validate_logo_url("ftp://cdn.example.com/logo.png").is_err());
        assert!(validate_logo_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn logo_url_invalid_rejected() {
        assert!(validate_logo_url("not-a-url").is_err());
        assert!(validate_logo_url("https://").is_err());
    }

    // ---- apply_branding_update integration tests ----

    async fn seed_user_and_restaurant(pool: &PgPool, email: &str, slug: &str) -> (i64, i64) {
        sqlx::query("INSERT INTO users (email, password_hash) VALUES ($1, 'x')")
            .bind(email)
            .execute(pool)
            .await
            .unwrap();
        let owner_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
            .bind(email)
            .fetch_one(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO restaurants (owner_id, name, slug, is_published)              VALUES ($1, $2, $3, false)",
        )
        .bind(owner_id)
        .bind(slug) // reuse slug as name for brevity
        .bind(slug)
        .execute(pool)
        .await
        .unwrap();
        let restaurant_id: i64 = sqlx::query_scalar("SELECT id FROM restaurants WHERE slug = $1")
            .bind(slug)
            .fetch_one(pool)
            .await
            .unwrap();
        (owner_id, restaurant_id)
    }

    /// Cross-tenant rejection: owner2 cannot update owner1's restaurant.
    #[sqlx::test(migrations = "./migrations")]
    async fn branding_update_cross_tenant_rejected(pool: PgPool) {
        let (_, r1_id) = seed_user_and_restaurant(&pool, "brand-a@test", "brand-a").await;
        let (owner2_id, _) = seed_user_and_restaurant(&pool, "brand-b@test", "brand-b").await;

        let result = apply_branding_update(&pool, r1_id, owner2_id, Some("#ff0000"), None)
            .await
            .unwrap();
        assert_eq!(
            result.rows_affected(),
            0,
            "cross-tenant update must affect 0 rows"
        );

        // Confirm r1 is unmodified.
        let stored: Option<String> =
            sqlx::query_scalar("SELECT accent_color FROM restaurants WHERE id = $1")
                .bind(r1_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(stored.is_none(), "r1 accent_color must remain NULL");
    }

    /// Authorized update: the correct owner can update their own restaurant.
    #[sqlx::test(migrations = "./migrations")]
    async fn branding_update_authorized_succeeds(pool: PgPool) {
        let (owner_id, r_id) = seed_user_and_restaurant(&pool, "brand-c@test", "brand-c").await;

        let result = apply_branding_update(
            &pool,
            r_id,
            owner_id,
            Some("#336699"),
            Some("https://example.com/logo.png"),
        )
        .await
        .unwrap();
        assert_eq!(
            result.rows_affected(),
            1,
            "authorized update must affect 1 row"
        );

        let (ac, lu): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT accent_color, logo_url FROM restaurants WHERE id = $1")
                .bind(r_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(ac.as_deref(), Some("#336699"));
        assert_eq!(lu.as_deref(), Some("https://example.com/logo.png"));
    }

    /// ID guard: a sibling restaurant owned by the same owner is not affected.
    #[sqlx::test(migrations = "./migrations")]
    async fn branding_update_id_guard_isolates_sibling(pool: PgPool) {
        let (owner_id, r1_id) = seed_user_and_restaurant(&pool, "brand-d@test", "brand-d1").await;
        // Second restaurant for the same owner — different id.
        sqlx::query(
            "INSERT INTO restaurants (owner_id, name, slug, is_published)              VALUES ($1, 'brand-d2', 'brand-d2', false)",
        )
        .bind(owner_id)
        .execute(&pool)
        .await
        .unwrap();
        let r2_id: i64 = sqlx::query_scalar("SELECT id FROM restaurants WHERE slug = 'brand-d2'")
            .fetch_one(&pool)
            .await
            .unwrap();

        // Update r1; r2 must remain untouched.
        let result = apply_branding_update(&pool, r1_id, owner_id, Some("#aabbcc"), None)
            .await
            .unwrap();
        assert_eq!(result.rows_affected(), 1);

        let r2_accent: Option<String> =
            sqlx::query_scalar("SELECT accent_color FROM restaurants WHERE id = $1")
                .bind(r2_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            r2_accent.is_none(),
            "sibling restaurant must not be modified"
        );
    }
}
