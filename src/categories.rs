use crate::auth::{AppState, new_csrf_token, verify_csrf_token};
use crate::restaurants::require_auth;
use askama::Template;
use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use object_store::path::Path as S3Path;
use serde::Deserialize;
use tower_sessions::Session;

struct CategoryRow {
    id: i64,
    name: String,
}

#[derive(Template)]
#[template(path = "category_list.html")]
struct CategoryListPage {
    restaurant_id: i64,
    restaurant_name: String,
    categories: Vec<CategoryRow>,
}

#[derive(Template)]
#[template(path = "category_new.html")]
struct CategoryNewPage {
    restaurant_id: i64,
    restaurant_name: String,
    token: String,
}

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
    let restaurant_name = require_restaurant_owner(&state.pool, restaurant_id, user_id).await?;

    let categories: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, name FROM menu_categories WHERE restaurant_id = $1 ORDER BY sort_order, id",
    )
    .bind(restaurant_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    let categories_view: Vec<CategoryRow> = categories
        .into_iter()
        .map(|(id, name)| CategoryRow { id, name })
        .collect();
    let html = CategoryListPage {
        restaurant_id,
        restaurant_name,
        categories: categories_view,
    }
    .render()
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "template error"))?;
    Ok(Html(html))
}

pub async fn new_form(
    State(state): State<AppState>,
    session: Session,
    Path(restaurant_id): Path<i64>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    let restaurant_name = require_restaurant_owner(&state.pool, restaurant_id, user_id).await?;
    let token = new_csrf_token(&session).await?;
    let html = CategoryNewPage {
        restaurant_id,
        restaurant_name,
        token,
    }
    .render()
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "template error"))?;
    Ok(Html(html))
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
        return Err((
            StatusCode::BAD_REQUEST,
            "category name must be 120 characters or fewer",
        ));
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
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "failed to create category",
        )
    })?;

    Ok(Redirect::to(&format!(
        "/restaurants/{restaurant_id}/categories"
    )))
}

/// Verifies the session user owns the restaurant containing this category.
/// Returns (restaurant_id, category_name).
async fn require_category_owner_by_id(
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

/// Production SQL for renaming a category, shared by the handler and tests.
async fn apply_category_name_update(
    pool: &sqlx::PgPool,
    category_id: i64,
    owner_id: i64,
    name: &str,
) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
    sqlx::query(
        "UPDATE menu_categories SET name = $1
         FROM restaurants r
         WHERE menu_categories.id = $2
           AND menu_categories.restaurant_id = r.id
           AND r.owner_id = $3",
    )
    .bind(name)
    .bind(category_id)
    .bind(owner_id)
    .execute(pool)
    .await
}

/// Production SQL for deleting a category with photo cleanup, shared by the handler and tests.
/// Returns the photo_url values of items that were cascaded away so callers can clean up S3.
async fn apply_category_delete(
    pool: &sqlx::PgPool,
    category_id: i64,
    owner_id: i64,
) -> Result<(sqlx::postgres::PgQueryResult, Vec<Option<String>>), sqlx::Error> {
    // Collect item photo_urls before the cascade removes the rows.
    let photo_urls: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT mi.photo_url
         FROM menu_items mi
         JOIN menu_categories mc ON mc.id = mi.category_id
         JOIN restaurants r ON r.id = mc.restaurant_id
         WHERE mc.id = $1 AND r.owner_id = $2",
    )
    .bind(category_id)
    .bind(owner_id)
    .fetch_all(pool)
    .await?;

    let result = sqlx::query(
        "DELETE FROM menu_categories
         USING restaurants r
         WHERE menu_categories.id = $1
           AND menu_categories.restaurant_id = r.id
           AND r.owner_id = $2",
    )
    .bind(category_id)
    .bind(owner_id)
    .execute(pool)
    .await?;

    Ok((result, photo_urls))
}

#[derive(Template)]
#[template(path = "category_edit.html")]
struct CategoryEditPage {
    id: i64,
    restaurant_id: i64,
    name: String,
    edit_token: String,
    delete_token: String,
}

#[derive(Deserialize)]
pub struct EditCategoryForm {
    name: String,
    authenticity_token: String,
}

#[derive(Deserialize)]
pub struct DeleteCategoryForm {
    authenticity_token: String,
}

pub async fn edit_form(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
) -> Result<Html<String>, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    let (restaurant_id, name) = require_category_owner_by_id(&state.pool, id, user_id).await?;
    let edit_token = new_csrf_token(&session).await?;
    let delete_token = new_csrf_token(&session).await?;
    let html = CategoryEditPage {
        id,
        restaurant_id,
        name,
        edit_token,
        delete_token,
    }
    .render()
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "template error"))?;
    Ok(Html(html))
}

pub async fn edit(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
    Form(form): Form<EditCategoryForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    verify_csrf_token(&session, &form.authenticity_token).await?;
    let (restaurant_id, _) = require_category_owner_by_id(&state.pool, id, user_id).await?;

    let name = form.name.trim();
    if name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "category name must not be empty"));
    }
    if name.len() > 120 {
        return Err((
            StatusCode::BAD_REQUEST,
            "category name must be 120 characters or fewer",
        ));
    }

    let result = apply_category_name_update(&state.pool, id, user_id, name)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to update category",
            )
        })?;

    if result.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "category not found"));
    }

    Ok(Redirect::to(&format!(
        "/restaurants/{restaurant_id}/categories"
    )))
}

pub async fn delete(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
    Form(form): Form<DeleteCategoryForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;
    verify_csrf_token(&session, &form.authenticity_token).await?;
    let (restaurant_id, _) = require_category_owner_by_id(&state.pool, id, user_id).await?;

    let (result, photo_urls) = apply_category_delete(&state.pool, id, user_id)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to delete category",
            )
        })?;

    if result.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "category not found"));
    }

    // Best-effort cleanup of S3 objects for items that were cascade-deleted.
    for url in photo_urls.into_iter().flatten() {
        if let Some(key) = url.strip_prefix(&format!("{}/", state.s3_public_base))
            && let Err(e) = state.s3.delete(&S3Path::from(key)).await
        {
            eprintln!("[categories] warning: failed to delete item photo {key}: {e}");
        }
    }

    Ok(Redirect::to(&format!(
        "/restaurants/{restaurant_id}/categories"
    )))
}

#[cfg(test)]
mod tests {
    use super::{apply_category_delete, apply_category_name_update};
    use sqlx::PgPool;

    async fn seed_user_restaurant_category(
        pool: &PgPool,
        email: &str,
        slug: &str,
    ) -> (i64, i64, i64) {
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
            "INSERT INTO restaurants (owner_id, name, slug, is_published) VALUES ($1, $2, $3, false)",
        )
        .bind(owner_id)
        .bind(slug)
        .bind(slug)
        .execute(pool)
        .await
        .unwrap();
        let restaurant_id: i64 =
            sqlx::query_scalar("SELECT id FROM restaurants WHERE owner_id = $1")
                .bind(owner_id)
                .fetch_one(pool)
                .await
                .unwrap();
        sqlx::query(
            "INSERT INTO menu_categories (restaurant_id, name, sort_order) VALUES ($1, 'Starters', 0)",
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
        (owner_id, restaurant_id, category_id)
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn category_edit_authorized_succeeds(pool: PgPool) {
        let (owner_id, _, cat_id) =
            seed_user_restaurant_category(&pool, "edit-ok@test", "edit-ok").await;
        let result = apply_category_name_update(&pool, cat_id, owner_id, "Mains")
            .await
            .unwrap();
        assert_eq!(result.rows_affected(), 1);
        let name: String = sqlx::query_scalar("SELECT name FROM menu_categories WHERE id = $1")
            .bind(cat_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "Mains");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn category_edit_cross_tenant_rejected(pool: PgPool) {
        let (_, _, cat_id) =
            seed_user_restaurant_category(&pool, "edit-owner@test", "edit-owner").await;
        let (other_owner_id, _, _) =
            seed_user_restaurant_category(&pool, "edit-other@test", "edit-other").await;
        let result = apply_category_name_update(&pool, cat_id, other_owner_id, "Hacked")
            .await
            .unwrap();
        assert_eq!(result.rows_affected(), 0);
        let name: String = sqlx::query_scalar("SELECT name FROM menu_categories WHERE id = $1")
            .bind(cat_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "Starters");
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn category_delete_authorized_succeeds(pool: PgPool) {
        let (owner_id, _, cat_id) =
            seed_user_restaurant_category(&pool, "del-ok@test", "del-ok").await;
        let (result, _) = apply_category_delete(&pool, cat_id, owner_id)
            .await
            .unwrap();
        assert_eq!(result.rows_affected(), 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_categories WHERE id = $1")
            .bind(cat_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn category_delete_cross_tenant_rejected(pool: PgPool) {
        let (_, _, cat_id) =
            seed_user_restaurant_category(&pool, "del-owner@test", "del-owner").await;
        let (other_owner_id, _, _) =
            seed_user_restaurant_category(&pool, "del-other@test", "del-other").await;
        let (result, _) = apply_category_delete(&pool, cat_id, other_owner_id)
            .await
            .unwrap();
        assert_eq!(result.rows_affected(), 0);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_categories WHERE id = $1")
            .bind(cat_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }
}
