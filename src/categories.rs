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

/// Deletes a category, collecting item photo_urls atomically within a Read Committed transaction.
///
/// Acquires FOR UPDATE locks on the category row (with ownership check via JOIN on restaurants)
/// and on all item rows in the category. The restaurant row is also locked as a side-effect of
/// the JOIN; this serializes concurrent deletes of different categories in the same restaurant.
/// The item locks block concurrent photo uploads from replacing photo_url between collection
/// and cascade deletion.
///
/// Returns `None` when the category is not found or not owned by `owner_id`.
/// Returns `Some((restaurant_id, photo_urls))` on success; `photo_urls` may contain `None`
/// entries for items without photos.
async fn apply_category_delete(
    pool: &sqlx::PgPool,
    category_id: i64,
    owner_id: i64,
) -> Result<Option<(i64, Vec<Option<String>>)>, sqlx::Error> {
    let mut tx = pool.begin().await?;

    // Lock the category row and verify ownership in one step.
    let restaurant_id: Option<i64> = sqlx::query_scalar(
        "SELECT mc.restaurant_id
         FROM menu_categories mc
         JOIN restaurants r ON r.id = mc.restaurant_id
         WHERE mc.id = $1 AND r.owner_id = $2
         FOR UPDATE",
    )
    .bind(category_id)
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await?;

    let restaurant_id = match restaurant_id {
        Some(id) => id,
        None => {
            tx.rollback().await.ok();
            return Ok(None);
        }
    };

    // Lock existing item rows and collect their photo_urls.
    // Locks prevent a concurrent upload from replacing a photo_url between here and DELETE.
    let photo_urls: Vec<Option<String>> =
        sqlx::query_scalar("SELECT photo_url FROM menu_items WHERE category_id = $1 FOR UPDATE")
            .bind(category_id)
            .fetch_all(&mut *tx)
            .await?;

    // Ownership already confirmed by the FOR UPDATE lock above.
    sqlx::query("DELETE FROM menu_categories WHERE id = $1")
        .bind(category_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(Some((restaurant_id, photo_urls)))
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

    let (restaurant_id, photo_urls) = apply_category_delete(&state.pool, id, user_id)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to delete category",
            )
        })?
        .ok_or((StatusCode::NOT_FOUND, "category not found"))?;

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
    async fn category_delete_returns_item_photo_urls(pool: PgPool) {
        let (owner_id, _, cat_id) =
            seed_user_restaurant_category(&pool, "del-photos@test", "del-photos").await;

        // Seed two items: one with a photo, one without.
        sqlx::query(
            "INSERT INTO menu_items (category_id, name, price_cents, sort_order, photo_url)
             VALUES ($1, 'Soup', 500, 0, 'http://s3/bucket/item-1.jpg'),
                    ($1, 'Bread', 200, 1, NULL)",
        )
        .bind(cat_id)
        .execute(&pool)
        .await
        .unwrap();

        let result = apply_category_delete(&pool, cat_id, owner_id)
            .await
            .unwrap();

        let (_, mut photo_urls) = result.expect("delete should succeed");
        photo_urls.sort();
        assert_eq!(
            photo_urls,
            vec![None, Some("http://s3/bucket/item-1.jpg".to_string())]
        );

        // Category and both items should be gone.
        let cat_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM menu_categories WHERE id = $1")
                .bind(cat_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cat_count, 0);

        let item_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM menu_items WHERE category_id = $1")
                .bind(cat_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(item_count, 0);
    }

    #[sqlx::test(migrations = "./migrations")]
    async fn category_delete_cross_tenant_rejected_with_items(pool: PgPool) {
        let (_, _, cat_id) =
            seed_user_restaurant_category(&pool, "del-owner@test", "del-owner").await;
        sqlx::query(
            "INSERT INTO menu_items (category_id, name, price_cents, sort_order, photo_url)
             VALUES ($1, 'Target', 100, 0, 'http://s3/bucket/target.jpg')",
        )
        .bind(cat_id)
        .execute(&pool)
        .await
        .unwrap();

        let (other_owner_id, _, _) =
            seed_user_restaurant_category(&pool, "del-other@test", "del-other").await;

        // Cross-tenant attempt must return None (not found) and leave data intact.
        let result = apply_category_delete(&pool, cat_id, other_owner_id)
            .await
            .unwrap();
        assert!(result.is_none(), "cross-tenant delete must return None");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_categories WHERE id = $1")
            .bind(cat_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);

        let item_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM menu_items WHERE category_id = $1")
                .bind(cat_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(item_count, 1);
    }
    /// Verifies that FOR UPDATE on item rows blocks concurrent photo uploads.
    ///
    /// A delete transaction acquires item row locks before collecting URLs and committing.
    /// A concurrent upload UPDATE waits for those locks; when the delete commits the item is
    /// gone, so the upload returns 0 rows. This test would fail if the FOR UPDATE were removed.
    #[sqlx::test(migrations = "./migrations")]
    async fn category_delete_for_update_blocks_concurrent_upload(pool: PgPool) {
        let (owner_id, _, cat_id) =
            seed_user_restaurant_category(&pool, "lock-test@test", "lock-test").await;
        sqlx::query(
            "INSERT INTO menu_items (category_id, name, price_cents, sort_order, photo_url)
             VALUES ($1, 'Item', 100, 0, 'http://s3/bucket/old.jpg')",
        )
        .bind(cat_id)
        .execute(&pool)
        .await
        .unwrap();
        let item_id: i64 = sqlx::query_scalar("SELECT id FROM menu_items WHERE category_id = $1")
            .bind(cat_id)
            .fetch_one(&pool)
            .await
            .unwrap();

        // Begin the delete transaction and acquire FOR UPDATE locks on category + items.
        let mut tx = pool.begin().await.unwrap();
        sqlx::query_scalar::<_, i64>(
            "SELECT mc.restaurant_id
             FROM menu_categories mc
             JOIN restaurants r ON r.id = mc.restaurant_id
             WHERE mc.id = $1 AND r.owner_id = $2
             FOR UPDATE",
        )
        .bind(cat_id)
        .bind(owner_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let collected: Vec<Option<String>> = sqlx::query_scalar(
            "SELECT photo_url FROM menu_items WHERE category_id = $1 FOR UPDATE",
        )
        .bind(cat_id)
        .fetch_all(&mut *tx)
        .await
        .unwrap();
        // Collected before any concurrent upload — must still be the original URL.
        assert_eq!(
            collected,
            vec![Some("http://s3/bucket/old.jpg".to_string())]
        );

        // Spawn a concurrent task that tries to replace the photo_url.
        // It will block on the item row lock held by tx.
        let pool2 = pool.clone();
        let upload_handle = tokio::task::spawn(async move {
            sqlx::query(
                "UPDATE menu_items SET photo_url = 'http://s3/bucket/new.jpg' WHERE id = $1",
            )
            .bind(item_id)
            .execute(&pool2)
            .await
            .unwrap()
            .rows_affected()
        });

        // Sleep briefly so the upload task can get scheduled and block on the PG lock.
        tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;

        // Complete the delete — cascade removes the item, then commit releases locks.
        sqlx::query("DELETE FROM menu_categories WHERE id = $1")
            .bind(cat_id)
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        // Upload unblocks: item was deleted while it waited → 0 rows affected.
        // If FOR UPDATE were absent, the upload could slip in and return 1, failing this assert.
        let upload_rows = upload_handle.await.unwrap();
        assert_eq!(
            upload_rows, 0,
            "upload blocked by FOR UPDATE then lost the race: item deleted before update committed"
        );
    }
}
