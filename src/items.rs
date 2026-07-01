use crate::auth::{new_csrf_token, verify_csrf_token, AppState};
use crate::restaurants::require_auth;
use askama::Template;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use serde::Deserialize;
use tower_sessions::Session;

struct ItemRow {
    id: i64,
    name: String,
    description: Option<String>,
    price: String,
    is_available: bool,
}

#[derive(Template)]
#[template(path = "item_list.html")]
struct ItemListPage {
    category_id: i64,
    restaurant_id: i64,
    category_name: String,
    items: Vec<ItemRow>,
}

#[derive(Template)]
#[template(path = "item_new.html")]
struct ItemNewPage {
    category_id: i64,
    category_name: String,
    token: String,
}

#[derive(Template)]
#[template(path = "item_edit.html")]
struct ItemEditPage {
    item_id: i64,
    category_id: i64,
    name: String,
    description: String,
    price_cents: i32,
    is_available: bool,
    edit_token: String,
    toggle_token: String,
    delete_token: String,
    photo_token: String,
    photo_url: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateItemForm {
    name: String,
    description: String,
    price_cents: String,
    authenticity_token: String,
}

#[derive(Deserialize)]
pub struct EditItemForm {
    name: String,
    description: String,
    price_cents: String,
    authenticity_token: String,
}

#[derive(Deserialize)]
pub struct TokenForm {
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

/// Verifies the session user owns the restaurant containing this item.
/// Returns (category_id, name, description, price_cents, is_available).
pub async fn require_item_owner(
    pool: &sqlx::PgPool,
    item_id: i64,
    user_id: i64,
) -> Result<(i64, String, Option<String>, i32, bool), (StatusCode, &'static str)> {
    let row: Option<(i64, String, Option<String>, i32, bool)> = sqlx::query_as(
        "SELECT mi.category_id, mi.name, mi.description, mi.price_cents, mi.is_available
         FROM menu_items mi
         JOIN menu_categories mc ON mc.id = mi.category_id
         JOIN restaurants r ON r.id = mc.restaurant_id
         WHERE mi.id = $1 AND r.owner_id = $2",
    )
    .bind(item_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    row.ok_or((StatusCode::NOT_FOUND, "item not found"))
}

pub async fn list(
    State(state): State<AppState>,
    session: Session,
    Path(category_id): Path<i64>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    let (restaurant_id, category_name) =
        require_category_owner(&state.pool, category_id, user_id).await?;

    let items: Vec<(i64, String, Option<String>, i32, bool)> = sqlx::query_as(
        "SELECT id, name, description, price_cents, is_available
         FROM menu_items WHERE category_id = $1 ORDER BY sort_order, id",
    )
    .bind(category_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    let items_view: Vec<ItemRow> = items
        .into_iter()
        .map(|(id, name, description, price_cents, is_available)| ItemRow {
            id,
            name,
            description,
            price: format!("{:.2}", price_cents as f64 / 100.0),
            is_available,
        })
        .collect();
    let html = ItemListPage { category_id, restaurant_id, category_name, items: items_view }
        .render()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "template error"))?;
    Ok(Html(html))
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
    let html = ItemNewPage { category_id, category_name, token }
        .render()
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "template error"))?;
    Ok(Html(html))
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

pub async fn edit_form(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<i64>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    let (category_id, name, description, price_cents, is_available) =
        require_item_owner(&state.pool, item_id, user_id).await?;

    let edit_token = new_csrf_token(&session).await?;
    let toggle_token = new_csrf_token(&session).await?;
    let delete_token = new_csrf_token(&session).await?;
    let photo_token = new_csrf_token(&session).await?;

    let photo_url: Option<String> = sqlx::query_scalar(
        "SELECT photo_url FROM menu_items WHERE id = $1",
    )
    .bind(item_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?
    .flatten();

    let description_str = description.unwrap_or_default();
    let html = ItemEditPage {
        item_id,
        category_id,
        name,
        description: description_str,
        price_cents,
        is_available,
        edit_token,
        toggle_token,
        delete_token,
        photo_token,
        photo_url,
    }
    .render()
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "template error"))?;
    Ok(Html(html))
}

pub async fn edit(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<i64>,
    Form(form): Form<EditItemForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    verify_csrf_token(&session, &form.authenticity_token).await?;

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

    // Single-statement guarded write: UPDATE only succeeds if item belongs to owner's restaurant.
    let category_id: Option<i64> = sqlx::query_scalar(
        "UPDATE menu_items
         SET name = $3, description = $4, price_cents = $5
         FROM menu_categories mc
         JOIN restaurants r ON r.id = mc.restaurant_id
         WHERE menu_items.id = $1
           AND menu_items.category_id = mc.id
           AND r.owner_id = $2
         RETURNING menu_items.category_id",
    )
    .bind(item_id)
    .bind(user_id)
    .bind(name)
    .bind(description_opt)
    .bind(price_cents)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to update item"))?;

    let category_id = category_id.ok_or((StatusCode::NOT_FOUND, "item not found"))?;
    Ok(Redirect::to(&format!("/categories/{category_id}/items")))
}

pub async fn delete(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<i64>,
    Form(form): Form<TokenForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    verify_csrf_token(&session, &form.authenticity_token).await?;

    // Guarded DELETE: only removes the item if it belongs to owner's restaurant.
    let category_id: Option<i64> = sqlx::query_scalar(
        "DELETE FROM menu_items
         USING menu_categories mc
         JOIN restaurants r ON r.id = mc.restaurant_id
         WHERE menu_items.id = $1
           AND menu_items.category_id = mc.id
           AND r.owner_id = $2
         RETURNING menu_items.category_id",
    )
    .bind(item_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to delete item"))?;

    let category_id = category_id.ok_or((StatusCode::NOT_FOUND, "item not found"))?;
    Ok(Redirect::to(&format!("/categories/{category_id}/items")))
}

pub async fn toggle(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<i64>,
    Form(form): Form<TokenForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    verify_csrf_token(&session, &form.authenticity_token).await?;

    // Guarded UPDATE: flip is_available only if item belongs to owner's restaurant.
    let category_id: Option<i64> = sqlx::query_scalar(
        "UPDATE menu_items
         SET is_available = NOT menu_items.is_available
         FROM menu_categories mc
         JOIN restaurants r ON r.id = mc.restaurant_id
         WHERE menu_items.id = $1
           AND menu_items.category_id = mc.id
           AND r.owner_id = $2
         RETURNING menu_items.category_id",
    )
    .bind(item_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to toggle item"))?;

    let category_id = category_id.ok_or((StatusCode::NOT_FOUND, "item not found"))?;
    Ok(Redirect::to(&format!("/categories/{category_id}/items")))
}
