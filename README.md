# QR Menu App

A multi-tenant SaaS that lets restaurant owners create and manage a digital
menu accessible via QR code — no app install required for customers, no
reprinting when prices change.

## Stack

- **Language**: Pure Rust (edition 2024)
- **Web framework**: Axum 0.8
- **Templates**: Askama (compile-time checked, server-rendered HTML)
- **Database**: PostgreSQL 16 via sqlx (with sqlx migrations)
- **Sessions**: tower-sessions + tower-sessions-sqlx-store (Postgres-backed)
- **Auth**: argon2id password hashing, session rotation on login, CSRF protection
- **Image storage**: S3-compatible object storage (MinIO in local dev)
- **QR generation**: `qrcode` crate — SVG/PNG on demand
- **Rate limiting**: in-process DashMap-based IP rate limiter on login/signup

## Local Development

### Prerequisites

- Docker + Docker Compose
- Rust toolchain (stable, edition 2024, >= 1.85)

### Quick start

```bash
cp .env.example .env
# Edit .env if needed (defaults work with the compose stack as-is)
docker compose up --build
```

The app binds to `http://localhost:3000`. Before photo uploads will work,
create the `qr-menu-photos` bucket in the MinIO console at
`http://localhost:9001` (default credentials: `minioadmin` / `minioadmin`).

### Environment variables

| Variable | Required | Default | Description |
|---|---|---|---|
| `DATABASE_URL` | Yes | — | Postgres connection string |
| `S3_ENDPOINT` | Yes | — | S3-compatible endpoint URL |
| `BUCKET` | Yes | — | S3 bucket name for photo uploads |
| `AWS_ACCESS_KEY_ID` | Yes | — | S3 access key |
| `AWS_SECRET_ACCESS_KEY` | Yes | — | S3 secret key |
| `BASE_URL` | No | `http://localhost:3000` | Public URL used in QR codes and photo links |
| `AWS_REGION` | No | `auto` | S3 region (`auto` works for MinIO) |

All required vars are validated at startup — the process panics with a combined
error listing every missing var rather than stopping at the first.

### Running without Docker

```bash
cargo build --release
DATABASE_URL=postgres://... S3_ENDPOINT=http://... BUCKET=... \
  AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=... \
  ./target/release/qr-menu-app
```

## Deployment

The project ships with a multi-stage Dockerfile:

- **Builder stage** (`rust:1.86-slim-bookworm`): builds the release binary with
  dependency layer caching.
- **Runtime stage** (`debian:bookworm-slim`): copies only the binary; runs as
  a non-root system user (`appuser`, UID 1001). Container healthcheck polls
  `GET /health` every 30 s.

For production, wire `DATABASE_URL` to a managed Postgres instance and
`S3_ENDPOINT`/`BUCKET` to your object storage provider. Set `BASE_URL` to
the public HTTPS URL behind your reverse proxy.

## CI

GitHub Actions runs on every push/PR to `main`:

1. `cargo build --locked`
2. `cargo clippy --locked -- -D warnings`
3. `cargo fmt -- --check`
4. `cargo test --locked`

Cargo registry and `target/` are cached keyed on `Cargo.lock`.
