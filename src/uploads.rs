use crate::auth::{verify_csrf_token, AppState};
use crate::items::require_item_owner;
use crate::restaurants::require_auth;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use tower_sessions::Session;

/// Maximum bytes accepted from a CSRF token multipart field.
/// Tokens are 64-char hex strings; 512 bytes is generous with no DoS risk.
const MAX_CSRF_FIELD_BYTES: usize = 512;

/// Accepts a multipart photo upload for a menu item.
/// Auth-gated + CSRF-protected. Full upload logic deferred to the next slice.
///
/// Field order contract (enforced here):
///   1. authenticity_token — must be the first field; bounded read (512 bytes max).
///   2. photo              — the file field; consumed/dropped in the stub.
/// Ordering: auth -> CSRF (bounded read) -> ownership -> file field.
pub async fn upload_photo(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<i64>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;

    // Require authenticity_token as the FIRST multipart field.
    let mut first_field = multipart
        .next_field()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid multipart body"))?
        .ok_or((StatusCode::BAD_REQUEST, "missing CSRF token field"))?;

    if first_field.name() != Some("authenticity_token") {
        return Err((StatusCode::BAD_REQUEST, "first multipart field must be authenticity_token"));
    }

    // Bounded chunked read -- reject with 413 before CSRF verification if the field
    // is oversized. This prevents an attacker from forcing large memory allocation
    // by sending a giant "authenticity_token" field value.
    let mut buf = Vec::with_capacity(64);
    loop {
        match first_field
            .chunk()
            .await
            .map_err(|_| (StatusCode::BAD_REQUEST, "failed to read CSRF field"))?
        {
            None => break,
            Some(chunk) => {
                if buf.len() + chunk.len() > MAX_CSRF_FIELD_BYTES {
                    return Err((StatusCode::PAYLOAD_TOO_LARGE, "CSRF token field too large"));
                }
                buf.extend_from_slice(&chunk);
            }
        }
    }
    let csrf_token = String::from_utf8(buf)
        .map_err(|_| (StatusCode::BAD_REQUEST, "CSRF token is not valid UTF-8"))?;

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
