use crate::auth::{AppState, new_csrf_token, verify_csrf_token};
use crate::restaurants::require_auth;
use askama::Template;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use object_store::path::Path as S3Path;
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
        .map(
            |(id, name, description, price_cents, is_available)| ItemRow {
                id,
                name,
                description,
                price: format!("{:.2}", price_cents as f64 / 100.0),
                is_available,
            },
        )
        .collect();
    let html = ItemListPage {
        category_id,
        restaurant_id,
        category_name,
        items: items_view,
    }
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
    let (_, category_name) = require_category_owner(&state.pool, category_id, user_id).await?;
    let token = new_csrf_token(&session).await?;
    let html = ItemNewPage {
        category_id,
        category_name,
        token,
    }
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
        return Err((
            StatusCode::BAD_REQUEST,
            "item name must be 120 characters or fewer",
        ));
    }

    let description = form.description.trim();
    let description_opt: Option<&str> = if description.is_empty() {
        None
    } else if description.len() > 500 {
        return Err((
            StatusCode::BAD_REQUEST,
            "description must be 500 characters or fewer",
        ));
    } else {
        Some(description)
    };

    let price_cents: i32 = form
        .price_cents
        .trim()
        .parse()
        .ok()
        .filter(|&p: &i32| p >= 0)
        .ok_or((
            StatusCode::BAD_REQUEST,
            "price must be a non-negative integer (cents)",
        ))?;

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

    let photo_url: Option<String> =
        sqlx::query_scalar("SELECT photo_url FROM menu_items WHERE id = $1")
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

/// Guarded UPDATE: applies name/description/price edit only if item belongs to the owner's restaurant.
/// Returns the item's category_id on success, or None if the item was not found / not owned.
async fn apply_item_edit(
    pool: &sqlx::PgPool,
    item_id: i64,
    user_id: i64,
    name: &str,
    description: Option<&str>,
    price_cents: i32,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar(
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
    .bind(description)
    .bind(price_cents)
    .fetch_optional(pool)
    .await
}

/// Guarded DELETE: removes the item only if it belongs to the owner's restaurant.
/// Returns (category_id, photo_url) on success, or None if not found / not owned.
async fn apply_item_delete(
    pool: &sqlx::PgPool,
    item_id: i64,
    user_id: i64,
) -> Result<Option<(i64, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        "DELETE FROM menu_items
         USING menu_categories mc
         JOIN restaurants r ON r.id = mc.restaurant_id
         WHERE menu_items.id = $1
           AND menu_items.category_id = mc.id
           AND r.owner_id = $2
         RETURNING menu_items.category_id, menu_items.photo_url",
    )
    .bind(item_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
}

/// Guarded UPDATE: flips is_available only if item belongs to the owner's restaurant.
/// Returns the item's category_id on success, or None if not found / not owned.
async fn apply_item_toggle(
    pool: &sqlx::PgPool,
    item_id: i64,
    user_id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar(
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
    .fetch_optional(pool)
    .await
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
        return Err((
            StatusCode::BAD_REQUEST,
            "item name must be 120 characters or fewer",
        ));
    }

    let description = form.description.trim();
    let description_opt: Option<&str> = if description.is_empty() {
        None
    } else if description.len() > 500 {
        return Err((
            StatusCode::BAD_REQUEST,
            "description must be 500 characters or fewer",
        ));
    } else {
        Some(description)
    };

    let price_cents: i32 = form
        .price_cents
        .trim()
        .parse()
        .ok()
        .filter(|&p: &i32| p >= 0)
        .ok_or((
            StatusCode::BAD_REQUEST,
            "price must be a non-negative integer (cents)",
        ))?;

    let category_id = apply_item_edit(
        &state.pool,
        item_id,
        user_id,
        name,
        description_opt,
        price_cents,
    )
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to update item"))?
    .ok_or((StatusCode::NOT_FOUND, "item not found"))?;
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

    let (category_id, photo_url) = apply_item_delete(&state.pool, item_id, user_id)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to delete item"))?
        .ok_or((StatusCode::NOT_FOUND, "item not found"))?;

    // Best-effort S3 cleanup: delete the orphaned photo object after the DB row is gone.
    if let Some(url) = photo_url
        && let Some(key) = url.strip_prefix(&format!("{}/", state.s3_public_base))
        && let Err(e) = state.s3.delete(&S3Path::from(key)).await
    {
        eprintln!(
            "[items] warning: failed to delete S3 object {key} for deleted item {item_id}: {e}"
        );
    }

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

    let category_id = apply_item_toggle(&state.pool, item_id, user_id)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to toggle item"))?
        .ok_or((StatusCode::NOT_FOUND, "item not found"))?;
    Ok(Redirect::to(&format!("/categories/{category_id}/items")))
}

#[cfg(test)]
mod tests {
    use super::{apply_item_delete, apply_item_edit, apply_item_toggle};
    use sqlx::PgPool;

    /// Seeds a complete owner → restaurant → category → item chain.
    /// Returns (owner_id, category_id, item_id).
    async fn seed_item(pool: &PgPool, suffix: &str) -> (i64, i64, i64) {
        let email = format!("owner-{suffix}@items.test");
        sqlx::query("INSERT INTO users (email, password_hash) VALUES ($1, 'x')")
            .bind(&email)
            .execute(pool)
            .await
            .unwrap();
        let owner_id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
            .bind(&email)
            .fetch_one(pool)
            .await
            .unwrap();
        let slug = format!("rest-{suffix}");
        sqlx::query(
            "INSERT INTO restaurants (owner_id, name, slug) VALUES ($1, 'Test Restaurant', $2)",
        )
        .bind(owner_id)
        .bind(&slug)
        .execute(pool)
        .await
        .unwrap();
        let restaurant_id: i64 = sqlx::query_scalar("SELECT id FROM restaurants WHERE slug = $1")
            .bind(&slug)
            .fetch_one(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO menu_categories (restaurant_id, name, sort_order) VALUES ($1, 'Mains', 0)",
        )
        .bind(restaurant_id)
        .execute(pool)
        .await
        .unwrap();
        let category_id: i64 =
            sqlx::query_scalar("SELECT id FROM menu_categories WHERE restaurant_id = $1")
                .bind(restaurant_id)
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO menu_items (category_id, name, description, price_cents, sort_order) \
             VALUES ($1, 'Burger', 'Classic burger', 1200, 0)",
        )
        .bind(category_id)
        .execute(pool)
        .await
        .unwrap();
        let item_id: i64 = sqlx::query_scalar("SELECT id FROM menu_items WHERE category_id = $1")
            .bind(category_id)
            .fetch_one(pool)
            .await
            .unwrap();
        (owner_id, category_id, item_id)
    }

    /// apply_item_edit with the correct owner updates all fields and returns the category_id.
    #[sqlx::test(migrations = "./migrations")]
    async fn item_edit_authorized_succeeds(pool: PgPool) {
        let (owner_id, category_id, item_id) = seed_item(&pool, "edit-ok").await;
        let result = apply_item_edit(
            &pool,
            item_id,
            owner_id,
            "Pizza",
            Some("Cheese pizza"),
            1500,
        )
        .await
        .unwrap();
        assert_eq!(result, Some(category_id));
        let (name, desc, price): (String, Option<String>, i32) =
            sqlx::query_as("SELECT name, description, price_cents FROM menu_items WHERE id = $1")
                .bind(item_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "Pizza");
        assert_eq!(desc.as_deref(), Some("Cheese pizza"));
        assert_eq!(price, 1500);
    }

    /// apply_item_edit with a different owner (who also owns a separate restaurant/item)
    /// returns None and leaves the target row unchanged.
    #[sqlx::test(migrations = "./migrations")]
    async fn item_edit_cross_tenant_rejected(pool: PgPool) {
        let (_owner_id, _category_id, item_id) = seed_item(&pool, "edit-xt-a").await;
        // other owner has their own restaurant + item — guard clause is actually exercised
        let (other_id, _other_cat, _other_item) = seed_item(&pool, "edit-xt-b").await;
        let result = apply_item_edit(&pool, item_id, other_id, "Hacked", None, 0)
            .await
            .unwrap();
        assert_eq!(result, None);
        let (name, desc): (String, Option<String>) =
            sqlx::query_as("SELECT name, description FROM menu_items WHERE id = $1")
                .bind(item_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(name, "Burger");
        assert_eq!(desc.as_deref(), Some("Classic burger"));
    }

    /// apply_item_delete with the correct owner removes the row and returns (category_id, None).
    #[sqlx::test(migrations = "./migrations")]
    async fn item_delete_authorized_succeeds(pool: PgPool) {
        let (owner_id, category_id, item_id) = seed_item(&pool, "del-ok").await;
        let result = apply_item_delete(&pool, item_id, owner_id).await.unwrap();
        assert_eq!(result, Some((category_id, None)));
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_items WHERE id = $1")
            .bind(item_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    /// apply_item_delete with a different owner (who also owns a separate restaurant/item)
    /// returns None and leaves the target row in place.
    #[sqlx::test(migrations = "./migrations")]
    async fn item_delete_cross_tenant_rejected(pool: PgPool) {
        let (_owner_id, _category_id, item_id) = seed_item(&pool, "del-xt-a").await;
        let (other_id, _other_cat, _other_item) = seed_item(&pool, "del-xt-b").await;
        let result = apply_item_delete(&pool, item_id, other_id).await.unwrap();
        assert_eq!(result, None);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_items WHERE id = $1")
            .bind(item_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    /// apply_item_toggle with the correct owner flips is_available and returns the category_id.
    #[sqlx::test(migrations = "./migrations")]
    async fn item_toggle_authorized_flips_availability(pool: PgPool) {
        let (owner_id, category_id, item_id) = seed_item(&pool, "tog-ok").await;
        // Default is_available is true per schema; first toggle sets it false
        let result = apply_item_toggle(&pool, item_id, owner_id).await.unwrap();
        assert_eq!(result, Some(category_id));
        let avail: bool = sqlx::query_scalar("SELECT is_available FROM menu_items WHERE id = $1")
            .bind(item_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(!avail, "toggle should have set is_available = false");
        // Second toggle restores true
        apply_item_toggle(&pool, item_id, owner_id).await.unwrap();
        let avail2: bool = sqlx::query_scalar("SELECT is_available FROM menu_items WHERE id = $1")
            .bind(item_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            avail2,
            "second toggle should have restored is_available = true"
        );
    }

    /// apply_item_toggle with a different owner (who also owns a separate restaurant/item)
    /// returns None and does not change availability.
    #[sqlx::test(migrations = "./migrations")]
    async fn item_toggle_cross_tenant_rejected(pool: PgPool) {
        let (_owner_id, _category_id, item_id) = seed_item(&pool, "tog-xt-a").await;
        let (other_id, _other_cat, _other_item) = seed_item(&pool, "tog-xt-b").await;
        let result = apply_item_toggle(&pool, item_id, other_id).await.unwrap();
        assert_eq!(result, None);
        let avail: bool = sqlx::query_scalar("SELECT is_available FROM menu_items WHERE id = $1")
            .bind(item_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            avail,
            "availability must be unchanged after cross-tenant toggle attempt"
        );
    }
}
