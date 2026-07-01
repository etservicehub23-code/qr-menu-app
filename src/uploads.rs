use crate::auth::{verify_csrf_token, AppState};
use crate::items::require_item_owner;
use crate::restaurants::require_auth;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use tower_sessions::Session;

/// Accepts a multipart photo upload for a menu item.
/// Auth-gated + CSRF-protected. Full upload logic deferred to the next slice.
pub async fn upload_photo(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<i64>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;

    // Extract CSRF token from multipart fields. The template puts
    // authenticity_token before the file field so it arrives first.
    let mut csrf_token: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid multipart body"))?
    {
        if field.name() == Some("authenticity_token") {
            csrf_token = Some(
                field
                    .text()
                    .await
                    .map_err(|_| (StatusCode::BAD_REQUEST, "failed to read CSRF field"))?,
            );
        }
        // Other fields (including the file) are dropped and auto-consumed.
    }

    let csrf_token = csrf_token.ok_or((StatusCode::BAD_REQUEST, "missing CSRF token"))?;
    verify_csrf_token(&session, &csrf_token).await?;

    // Verify the item belongs to the authenticated owner.
    require_item_owner(&state.pool, item_id, user_id).await?;

    // Actual S3 upload deferred to the next slice.
    Ok(Redirect::to(&format!("/items/{item_id}/edit")))
}
