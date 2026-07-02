use crate::auth::{verify_csrf_token, AppState};
use crate::items::require_item_owner;
use crate::restaurants::require_auth;
use axum::extract::{Multipart, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use object_store::path::Path as S3Path;
use tower_sessions::Session;
use uuid::Uuid;

/// Maximum bytes accepted from a CSRF token multipart field.
const MAX_CSRF_FIELD_BYTES: usize = 512;

/// Maximum bytes accepted for a photo upload (5 MB).
const MAX_PHOTO_BYTES: usize = 5 * 1024 * 1024;

/// Detect image type from magic bytes. Returns the file extension or None.
/// Accepted: JPEG, PNG, WebP. SVG and all other types are rejected.
fn detect_image_ext(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// Accepts a multipart photo upload for a menu item and stores it in S3.
///
/// Field order contract:
///   1. authenticity_token — CSRF token; bounded read (512 bytes max).
///   2. photo              — image file; streamed with 5 MB hard cap.
/// Extra fields beyond these two are rejected.
///
/// Ordering: auth -> CSRF (bounded read) -> ownership -> file read + upload.
pub async fn upload_photo(
    State(state): State<AppState>,
    session: Session,
    Path(item_id): Path<i64>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id = require_auth(&session).await?;

    // 1. Require authenticity_token as the FIRST field with a bounded read.
    let mut first_field = multipart
        .next_field()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid multipart body"))?
        .ok_or((StatusCode::BAD_REQUEST, "missing CSRF token field"))?;

    if first_field.name() != Some("authenticity_token") {
        return Err((StatusCode::BAD_REQUEST, "first multipart field must be authenticity_token"));
    }

    let mut csrf_buf = Vec::with_capacity(64);
    loop {
        match first_field
            .chunk()
            .await
            .map_err(|_| (StatusCode::BAD_REQUEST, "failed to read CSRF field"))?
        {
            None => break,
            Some(chunk) => {
                if csrf_buf.len() + chunk.len() > MAX_CSRF_FIELD_BYTES {
                    return Err((StatusCode::PAYLOAD_TOO_LARGE, "CSRF token field too large"));
                }
                csrf_buf.extend_from_slice(&chunk);
            }
        }
    }
    let csrf_token = String::from_utf8(csrf_buf)
        .map_err(|_| (StatusCode::BAD_REQUEST, "CSRF token is not valid UTF-8"))?;

    verify_csrf_token(&session, &csrf_token).await?;

    // 2. Ownership check before reading any file bytes.
    require_item_owner(&state.pool, item_id, user_id).await?;

    // 3. Read the photo field — must be the second field and named "photo".
    let mut photo_field = multipart
        .next_field()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid multipart body"))?
        .ok_or((StatusCode::BAD_REQUEST, "missing photo field"))?;

    if photo_field.name() != Some("photo") {
        return Err((StatusCode::BAD_REQUEST, "second multipart field must be named photo"));
    }

    // Stream photo bytes with a hard 5 MB cap.
    let mut photo_buf: Vec<u8> = Vec::new();
    loop {
        match photo_field
            .chunk()
            .await
            .map_err(|_| (StatusCode::BAD_REQUEST, "failed to read photo field"))?
        {
            None => break,
            Some(chunk) => {
                if photo_buf.len() + chunk.len() > MAX_PHOTO_BYTES {
                    return Err((StatusCode::PAYLOAD_TOO_LARGE, "photo exceeds 5 MB limit"));
                }
                photo_buf.extend_from_slice(&chunk);
            }
        }
    }

    if photo_buf.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "photo field is empty"));
    }

    // 4. MIME validation via magic bytes. Reject SVG and anything not JPEG/PNG/WebP.
    let ext = detect_image_ext(&photo_buf)
        .ok_or((StatusCode::UNPROCESSABLE_ENTITY, "unsupported image type; only JPEG, PNG, and WebP are accepted"))?;

    // 5. Reject unexpected extra fields.
    if multipart
        .next_field()
        .await
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid multipart body"))?
        .is_some()
    {
        return Err((StatusCode::BAD_REQUEST, "unexpected extra multipart fields"));
    }

    // 6. Upload to S3 with an app-generated key: menu-items/{item_id}/{uuid}.{ext}.
    let object_key = format!("menu-items/{item_id}/{}.{ext}", Uuid::new_v4());
    let s3_path = S3Path::from(object_key.as_str());
    let payload = object_store::PutPayload::from(photo_buf);
    state
        .s3
        .put(&s3_path, payload)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to upload photo to storage"))?;

    // 7. Store the public URL (not the raw key) so the template can render it as <img src>.
    //    s3_public_base is app-generated from trusted config, not from user input.
    let photo_url = format!("{}/{}", state.s3_public_base, object_key);
    sqlx::query("UPDATE menu_items SET photo_url = $1 WHERE id = $2")
        .bind(&photo_url)
        .execute(&state.pool)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to save photo reference"))?;

    Ok(Redirect::to(&format!("/items/{item_id}/edit")))
}
