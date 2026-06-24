use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{Form, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use serde::Deserialize;
use sqlx::PgPool;
use tower_sessions::Session;

const USER_ID_KEY: &str = "user_id";
const CSRF_TOKEN_KEY: &str = "csrf_token";
const MIN_PASSWORD_LEN: usize = 8;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
}

#[derive(Deserialize)]
pub struct AuthForm {
    email: String,
    password: String,
    authenticity_token: String,
}

#[derive(Deserialize)]
pub struct LogoutForm {
    authenticity_token: String,
}

/// Generates a 32-byte random token, stores it in the session, and returns the
/// hex-encoded string to embed in the form.
async fn new_csrf_token(session: &Session) -> Result<String, (StatusCode, &'static str)> {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let token: String = bytes
        .iter()
        .flat_map(|b| {
            let hi = b >> 4;
            let lo = b & 0xf;
            [
                char::from_digit(hi as u32, 16).unwrap(),
                char::from_digit(lo as u32, 16).unwrap(),
            ]
        })
        .collect();
    session
        .insert(CSRF_TOKEN_KEY, token.clone())
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to set CSRF token"))?;
    Ok(token)
}

/// Reads the stored CSRF token from the session, compares it to `submitted`,
/// and removes the token so it is single-use.
async fn verify_csrf_token(
    session: &Session,
    submitted: &str,
) -> Result<(), (StatusCode, &'static str)> {
    let stored: Option<String> = session
        .get(CSRF_TOKEN_KEY)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "session error"))?;
    let stored = stored.ok_or((StatusCode::FORBIDDEN, "missing CSRF token"))?;
    // Remove before comparing so an error response still invalidates the token.
    session
        .remove::<String>(CSRF_TOKEN_KEY)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "session error"))?;
    if stored != submitted {
        return Err((StatusCode::FORBIDDEN, "invalid CSRF token"));
    }
    Ok(())
}

pub async fn signup_form(session: Session) -> Result<Html<String>, (StatusCode, &'static str)> {
    let token = new_csrf_token(&session).await?;
    Ok(Html(format!(
        r#"<!doctype html><html><body>
<h1>Sign up</h1>
<form method="post" action="/signup">
<input type="hidden" name="authenticity_token" value="{token}">
<label>Email <input type="email" name="email" required></label><br>
<label>Password <input type="password" name="password" required minlength="8"></label><br>
<button type="submit">Sign up</button>
</form>
</body></html>"#
    )))
}

pub async fn login_form(session: Session) -> Result<Html<String>, (StatusCode, &'static str)> {
    let token = new_csrf_token(&session).await?;
    Ok(Html(format!(
        r#"<!doctype html><html><body>
<h1>Log in</h1>
<form method="post" action="/login">
<input type="hidden" name="authenticity_token" value="{token}">
<label>Email <input type="email" name="email" required></label><br>
<label>Password <input type="password" name="password" required></label><br>
<button type="submit">Log in</button>
</form>
</body></html>"#
    )))
}

pub async fn signup(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<AuthForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    verify_csrf_token(&session, &form.authenticity_token).await?;

    let email = form.email.trim();
    if email.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "email must not be empty"));
    }
    if form.password.len() < MIN_PASSWORD_LEN {
        return Err((StatusCode::BAD_REQUEST, "password must be at least 8 characters"));
    }

    let salt = SaltString::generate(&mut OsRng);
    let password_hash = Argon2::default()
        .hash_password(form.password.as_bytes(), &salt)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to hash password"))?
        .to_string();

    let result = sqlx::query_scalar::<_, i64>(
        "INSERT INTO users (email, password_hash) VALUES ($1, $2) RETURNING id",
    )
    .bind(email)
    .bind(&password_hash)
    .fetch_one(&state.pool)
    .await;

    let user_id = match result {
        Ok(id) => id,
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
            return Err((StatusCode::CONFLICT, "an account with that email already exists"));
        }
        Err(_) => {
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "failed to create account"));
        }
    };

    log_in_session(&session, user_id)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to start session"))?;

    Ok(Redirect::to("/"))
}

pub async fn login(
    State(state): State<AppState>,
    session: Session,
    Form(form): Form<AuthForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    verify_csrf_token(&session, &form.authenticity_token).await?;

    let email = form.email.trim();

    let row = sqlx::query_as::<_, (i64, String)>(
        "SELECT id, password_hash FROM users WHERE email = $1::citext",
    )
    .bind(email)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to look up account"))?;

    let Some((user_id, password_hash)) = row else {
        return Err((StatusCode::UNAUTHORIZED, "invalid email or password"));
    };

    let parsed_hash = PasswordHash::new(&password_hash)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to verify password"))?;
    if Argon2::default()
        .verify_password(form.password.as_bytes(), &parsed_hash)
        .is_err()
    {
        return Err((StatusCode::UNAUTHORIZED, "invalid email or password"));
    }

    log_in_session(&session, user_id)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to start session"))?;

    Ok(Redirect::to("/"))
}

pub async fn logout(
    session: Session,
    Form(form): Form<LogoutForm>,
) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    verify_csrf_token(&session, &form.authenticity_token).await?;
    session
        .flush()
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to end session"))?;
    Ok(Redirect::to("/login"))
}

pub async fn index(session: Session) -> Result<impl IntoResponse, (StatusCode, &'static str)> {
    let user_id: Option<i64> = session
        .get(USER_ID_KEY)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "failed to read session"))?;

    match user_id {
        Some(id) => {
            let token = new_csrf_token(&session).await?;
            Ok(Html(format!(
                r#"<p>Logged in as user #{id}.</p>
<form method="post" action="/logout">
<input type="hidden" name="authenticity_token" value="{token}">
<button type="submit">Log out</button>
</form>"#
            )))
        }
        None => Ok(Html(
            "<p>Not logged in. <a href=\"/login\">Log in</a> or <a href=\"/signup\">sign up</a>.</p>"
                .to_string(),
        )),
    }
}

async fn log_in_session(
    session: &Session,
    user_id: i64,
) -> Result<(), tower_sessions::session::Error> {
    // Rotate the session ID on login to prevent session fixation, then store
    // the authenticated user's id.
    session.cycle_id().await?;
    session.insert(USER_ID_KEY, user_id).await?;
    Ok(())
}
