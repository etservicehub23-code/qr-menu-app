use crate::auth::{new_csrf_token, verify_csrf_token, AppState};
use crate::restaurants::require_auth;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use serde::Deserialize;
use tower_sessions::Session;

#[derive(Deserialize)]
pub struct CreateCategoryForm {
    name: String,
    authenticity_token: String,
}

/// Verifies the authenticated user owns the given restaurant; returns the restaurant name.
async fn require_restaurant_owner(
    pool: &sqlx::PgPool,
    restaurant_id: i64,
    user_id: i64,
) -> Result<String, (StatusCode, &'static str)> {
    let name: Option<String> =
        sqlx::query_scalar("SELECT name FROM restaurants WHERE id = $1 AND owner_id = $2")
            .bind(restaurant_id)
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;
    name.ok_or((StatusCode::NOT_FOUND, "restaurant not found"))
}

pub async fn list(
    State(state): State<AppState>,
    session: Session,
    Path(restaurant_id): Path<i64>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    let restaurant_name =
        require_restaurant_owner(&state.pool, restaurant_id, user_id).await?;

    let categories: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, name FROM menu_categories WHERE restaurant_id = $1 ORDER BY sort_order, id",
    )
    .bind(restaurant_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    let list_html = if categories.is_empty() {
        "<p>No categories yet.</p>".to_string()
    } else {
        format!(
            "<ul>\n{}\n</ul>",
            categories
                .iter()
                .map(|(_, name)| format!("  <li>{name}</li>"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    Ok(Html(format!(
        r#"<!doctype html><html><body>
<h1>{restaurant_name} — Categories</h1>
{list_html}
<p><a href="/restaurants/{restaurant_id}/categories/new">+ New category</a></p>
<p><a href="/restaurants/{restaurant_id}">Back to restaurant</a></p>
</body></html>"#
    )))
}

pub async fn new_form(
    State(state): State<AppState>,
    session: Session,
    Path(restaurant_id): Path<i64>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    let restaurant_name =
        require_restaurant_owner(&state.pool, restaurant_id, user_id).await?;
    let token = new_csrf_token(&session).await?;
    Ok(Html(format!(
        r#"<!doctype html><html><body>
<h1>{restaurant_name} — New Category</h1>
<form method="post" action="/restaurants/{restaurant_id}/categories/new">
<input type="hidden" name="authenticity_token" value="{token}">
<label>Category name <input type="text" name="name" required maxlength="120"></label><br>
<button type="submit">Add Category</button>
</form>
<p><a href="/restaurants/{restaurant_id}/categories">Back to categories</a></p>
</body></html>"#
    )))
}

pub async fn create(
    State(state): State<AppState>,
    session: Session,
    Path(restaurant_id): Path<i64>,
    Form(form): Form<CreateCategoryForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    verify_csrf_token(&session, &form.authenticity_token).await?;
    require_restaurant_owner(&state.pool, restaurant_id, user_id).await?;

    let name = form.name.trim();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "category name must not be empty"));
    }
    if name.len() > 120 {
        return Err((StatusCode::BAD_REQUEST, "category name must be 120 characters or fewer"));
    }

    sqlx::query(
        "INSERT INTO menu_categories (restaurant_id, name, sort_order)
         VALUES ($1, $2,
             (SELECT COALESCE(MAX(sort_order) + 1, 0) FROM menu_categories WHERE restaurant_id = $1)
         )",
    )
    .bind(restaurant_id)
    .bind(name)
    .execute(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to create category"))?;

    Ok(Redirect::to(&format!("/restaurants/{restaurant_id}/categories")))
}
