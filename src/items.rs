use crate::auth::{new_csrf_token, verify_csrf_token, AppState};
use crate::escape::html_escape;
use crate::restaurants::require_auth;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use serde::Deserialize;
use tower_sessions::Session;

#[derive(Deserialize)]
pub struct CreateItemForm {
    name: String,
    description: String,
    price_cents: String,
    authenticity_token: String,
}

/// Verifies the session user owns the restaurant containing this category.
/// Returns (restaurant_id, category_name).
async fn require_category_owner(
    pool: &sqlx::PgPool,
    category_id: i64,
    user_id: i64,
) -> Result<(i64, String), (StatusCode, &'static str)> {
    let row: Option<(i64, String)> = sqlx::query_as(
        "SELECT mc.restaurant_id, mc.name
         FROM menu_categories mc
         JOIN restaurants r ON r.id = mc.restaurant_id
         WHERE mc.id = $1 AND r.owner_id = $2",
    )
    .bind(category_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    row.ok_or((StatusCode::NOT_FOUND, "category not found"))
}

pub async fn list(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<i64>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    let (restaurant_id, category_name) =
        require_category_owner(&state.pool, category_id, user_id).await?;

    let items: Vec<(i64, String, Option<String>, i32)> = sqlx::query_as(
        "SELECT id, name, description, price_cents
         FROM menu_items WHERE category_id = $1 ORDER BY sort_order, id",
    )
    .bind(category_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    let list_html = if items.is_empty() {
        "<p>No items yet.</p>".to_string()
    } else {
        format!(
            "<ul>\n{}\n</ul>",
            items
                .iter()
                .map(|(_, name, desc, price_cents)| {
                    let name_e = html_escape(name);
                    let desc_e = desc
                        .as_deref()
                        .map(html_escape)
                        .unwrap_or_default();
                    let price = format!("{:.2}", *price_cents as f64 / 100.0);
                    if desc_e.is_empty() {
                        format!("  <li><strong>{name_e}</strong> — €{price}</li>")
                    } else {
                        format!("  <li><strong>{name_e}</strong> — €{price}<br><small>{desc_e}</small></li>")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let category_name_escaped = html_escape(&category_name);
    Ok(Html(format!(
        r#"<!doctype html><html><body>
<h1>{category_name_escaped} — Items</h1>
{list_html}
<p><a href="/categories/{category_id}/items/new">+ New item</a></p>
<p><a href="/restaurants/{restaurant_id}/categories">Back to categories</a></p>
</body></html>"#
    )))
}

pub async fn new_form(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<i64>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    let (_, category_name) =
        require_category_owner(&state.pool, category_id, user_id).await?;
    let token = new_csrf_token(&session).await?;
    let category_name_escaped = html_escape(&category_name);
    Ok(Html(format!(
        r#"<!doctype html><html><body>
<h1>{category_name_escaped} — New Item</h1>
<form method="post" action="/categories/{category_id}/items/new">
<input type="hidden" name="authenticity_token" value="{token}">
<label>Item name <input type="text" name="name" required maxlength="120"></label><br>
<label>Description <textarea name="description" maxlength="500"></textarea></label><br>
<label>Price (cents, e.g. 1250 = €12.50) <input type="number" name="price_cents" required min="0" max="999999"></label><br>
<button type="submit">Add Item</button>
</form>
<p><a href="/categories/{category_id}/items">Back to items</a></p>
</body></html>"#
    )))
}

pub async fn create(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<i64>,
    Form(form): Form<CreateItemForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    verify_csrf_token(&session, &form.authenticity_token).await?;
    require_category_owner(&state.pool, category_id, user_id).await?;

    let name = form.name.trim();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "item name must not be empty"));
    }
    if name.len() > 120 {
        return Err((StatusCode::BAD_REQUEST, "item name must be 120 characters or fewer"));
    }

    let description = form.description.trim();
    let description_opt: Option<&str> = if description.is_empty() {
        None
    } else if description.len() > 500 {
        return Err((StatusCode::BAD_REQUEST, "description must be 500 characters or fewer"));
    } else {
        Some(description)
    };

    let price_cents: i32 = form
        .price_cents
        .trim()
        .parse()
        .ok()
        .filter(|&p: &i32| p >= 0)
        .ok_or((StatusCode::BAD_REQUEST, "price must be a non-negative integer (cents)"))?;

    sqlx::query(
        "INSERT INTO menu_items (category_id, name, description, price_cents, sort_order)
         VALUES ($1, $2, $3, $4,
             (SELECT COALESCE(MAX(sort_order) + 1, 0) FROM menu_items WHERE category_id = $1)
         )",
    )
    .bind(category_id)
    .bind(name)
    .bind(description_opt)
    .bind(price_cents)
    .execute(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to create item"))?;

    Ok(Redirect::to(&format!("/categories/{category_id}/items")))
}
