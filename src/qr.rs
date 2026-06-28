use crate::auth::{AppState, USER_ID_KEY};
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use qrcode::render::svg;
use qrcode::QrCode;
use tower_sessions::Session;

pub async fn qr_svg(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<i64>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id: i64 = session
        .get::<i64>(USER_ID_KEY)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "session error"))?
        .ok_or((StatusCode::UNAUTHORIZED, "you must be logged in"))?;

    let slug: Option<String> = sqlx::query_scalar(
        "SELECT slug FROM restaurants WHERE id = $1 AND owner_id = $2",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "database error"))?;

    let slug = slug.ok_or((StatusCode::NOT_FOUND, "restaurant not found"))?;

    let url = format!("{}/m/{}", state.base_url.trim_end_matches('/'), slug);

    let code = QrCode::new(url.as_bytes())
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to generate QR code"))?;

    let svg_string = code
        .render::<svg::Color>()
        .min_dimensions(200, 200)
        .build();

    Ok((
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"menu-qr.svg\"",
            ),
        ],
        svg_string,
    ))
}
