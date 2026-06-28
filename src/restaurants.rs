use crate::auth::{new_csrf_token, verify_csrf_token, AppState, USER_ID_KEY};
use crate::escape::html_escape;
use axum::extract::{Form, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use serde::Deserialize;
use tower_sessions::Session;

#[derive(Deserialize)]
pub struct CreateRestaurantForm {
    name: String,
    authenticity_token: String,
}

#[derive(Deserialize)]
pub struct TokenForm {
    authenticity_token: String,
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

    Err((StatusCode::CONFLICT, "could not generate a unique slug for this name"))
}

pub async fn new_form(
    session: Session,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let _user_id = require_auth(&session).await?;
    let token = new_csrf_token(&session).await?;
    Ok(Html(format!(
        r#"<!doctype html><html><body>
<h1>Create your restaurant</h1>
<form method="post" action="/restaurants/new">
<input type="hidden" name="authenticity_token" value="{token}">
<label>Restaurant name <input type="text" name="name" required maxlength="120"></label><br>
<button type="submit">Create</button>
</form>
<p><a href="/">Back</a></p>
</body></html>"#
    )))
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
        return Err((StatusCode::BAD_REQUEST, "restaurant name must be 120 characters or fewer"));
    }

    let base_slug = slugify(name);
    if base_slug.len() < 3 {
        return Err((StatusCode::BAD_REQUEST, "restaurant name must produce a valid URL slug (try adding more letters)"));
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
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to create restaurant"))?;

    Ok(Redirect::to(&format!("/restaurants/{restaurant_id}")))
}

pub async fn show(
    State(state): State<AppState>,
    session: Session,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;

    let row: Option<(String, String, bool)> = sqlx::query_as(
        "SELECT name, slug, is_published FROM restaurants WHERE id = $1 AND owner_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    let Some((name, slug, is_published)) = row else {
        return Err((StatusCode::NOT_FOUND, "restaurant not found"));
    };

    let token = new_csrf_token(&session).await?;
    let status = if is_published { "published" } else { "draft" };
    let toggle_label = if is_published { "Unpublish" } else { "Publish" };
    let name_escaped = html_escape(&name);
    let slug_escaped = html_escape(&slug);
    Ok(Html(format!(
        r#"<!doctype html><html><body>
<h1>{name_escaped}</h1>
<p>Slug: <code>{slug_escaped}</code> · Status: <strong>{status}</strong></p>
<p>Public menu: <a href="/m/{slug_escaped}">/m/{slug_escaped}</a></p>
<form method="post" action="/restaurants/{id}/publish" style="display:inline">
<input type="hidden" name="authenticity_token" value="{token}">
<button type="submit">{toggle_label}</button>
</form>
<p><a href="/restaurants/{id}/categories">Manage categories</a></p>
<p><a href="/restaurants/{id}/qr">Download QR code (SVG)</a></p>
<p><a href="/">Back</a></p>
</body></html>"#
    )))
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
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to toggle publish status"))?;

    if rows.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "restaurant not found"));
    }

    Ok(Redirect::to(&format!("/restaurants/{id}")))
}

/// Extracts the authenticated user_id from the session, or returns 401.
pub async fn require_auth(session: &Session) -> Result<i64, (StatusCode, &'static str)> {
    session
        .get::<i64>(USER_ID_KEY)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "session error"))?
        .ok_or((StatusCode::UNAUTHORIZED, "you must be logged in"))
}
