use crate::auth::{verify_csrf_token, AppState};
use crate::items::require_item_owner;
use crate::restaurants::require_auth;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use tower_sessions::Session;

/// Accepts a multipart photo upload for a menu item.
/// Auth-gated + CSRF-protected. Full upload logic deferred to the next slice.
///
/// Field order contract (enforced here):
///   1. authenticity_token — must be the first field; rejected immediately otherwise.
///   2. photo              — the file field; consumed/dropped in the stub.
/// This ordering ensures CSRF and ownership are verified before any file bytes are read,
/// bounding DoS exposure even when body limits are raised for large uploads.
pub async fn upload_photo(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<i64>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;

    // Require authenticity_token as the FIRST multipart field.
    // If the first field is absent or has a different name, reject before reading anything else.
    let first_field = multipart
        .next_field()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid multipart body"))?
        .ok_or((StatusCode::BAD_REQUEST, "missing CSRF token field"))?;

    if first_field.name() != Some("authenticity_token") {
        return Err((StatusCode::BAD_REQUEST, "first multipart field must be authenticity_token"));
    }

    let csrf_token = first_field
        .text()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "failed to read CSRF field"))?;

    verify_csrf_token(&session, &csrf_token).await?;

    // Ownership check before reading any file bytes.
    require_item_owner(&state.pool, item_id, user_id).await?;

    // Consume (drop) the file field without reading its bytes.
    // next_field() auto-drains the previous field before advancing the stream.
    let _file_field = multipart
        .next_field()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid multipart body"))?;

    // Actual S3 upload deferred to the next slice.
    Ok(Redirect::to(&format!("/items/{item_id}/edit")))
}
