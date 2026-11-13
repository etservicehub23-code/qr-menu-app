# QR Menu App

A multi-tenant SaaS that lets restaurant owners create and manage a digital
menu accessible via QR code — no app install required for customers, no
reprinting when prices change.

## Stack

- **Language**: Pure Rust (edition 2024, MSRV 1.88)
- **Web framework**: Axum 0.8
- **Templates**: Askama (compile-time checked, server-rendered HTML)
- **Database**: PostgreSQL 16 via sqlx (migrations run automatically on startup)
- **Sessions**: tower-sessions + tower-sessions-sqlx-store (Postgres-backed, 7-day sliding expiry)
- **Auth**: argon2id password hashing, session rotation on login, CSRF protection
- **Image storage**: S3-compatible object storage (MinIO in local dev)
- **QR generation**: `qrcode` crate — SVG on demand
- **Rate limiting**: in-process DashMap-based IP rate limiter on login/signup (10 req/15 min per IP; see note below on reverse proxies)

## Local Development

### Prerequisites

- Docker + Docker Compose
- Rust toolchain (stable >= 1.88)
- MinIO Client (`mc`) for bucket initialization — install from
  https://min.io/docs/minio/linux/reference/minio-mc.html

### Quick start

```bash
cp .env.example .env
# Edit .env if needed (defaults work with the compose stack as-is)
docker compose up --build -d
```

The app binds to `http://localhost:3000`.

**MinIO bucket setup (required before photo uploads work):**

```bash
# Point the MinIO client at the local instance
mc alias set qrmenu http://localhost:9000 minioadmin minioadmin

# Create the bucket (safe to re-run if it already exists)
mc mb --ignore-existing qrmenu/qr-menu-photos

# Allow anonymous download so photo image URLs load in the browser
mc anonymous set download qrmenu/qr-menu-photos
```

**Photo image URLs — hostname note:** Photo URLs are constructed as
`S3_ENDPOINT/BUCKET/filename` (e.g. `http://minio:9000/qr-menu-photos/...`).
Port 9000 is exposed to the host by docker-compose, but the hostname `minio`
only resolves inside Docker by default. To make photo images load in your
browser on the same machine, add this line to `/etc/hosts`:

```
127.0.0.1 minio
```

This is a local-dev convenience only. Production deployments should use a
browser-reachable object storage URL (see Deployment below).

### Environment variables

| Variable | Required | Default | Description |
|---|---|---|---|
| `DATABASE_URL` | Yes | — | Postgres connection string |
| `S3_ENDPOINT` | Yes | — | S3-compatible endpoint URL (also used as the base for public photo URLs) |
| `BUCKET` | Yes | — | S3 bucket name for photo uploads |
| `AWS_ACCESS_KEY_ID` | Yes | — | S3 access key |
| `AWS_SECRET_ACCESS_KEY` | Yes | — | S3 secret key |
| `BASE_URL` | No | `http://localhost:3000` | Public URL used in QR codes |
| `AWS_REGION` | No | `auto` | S3 region (`auto` works for MinIO) |

All required vars are validated at startup — the process panics with a combined
error listing every missing var rather than stopping at the first.

### Running without Docker

Requires a running Postgres instance and an S3-compatible store reachable from
the process. The binary does **not** auto-load a `.env` file; export vars
explicitly or use a wrapper script.

On first startup, sqlx applies migrations automatically. The database role
needs `CREATE` privilege on the database (sufficient for sqlx migrations and
for installing the trusted `citext` extension on PostgreSQL 16).

```bash
~/.cargo/bin/cargo build --release
export DATABASE_URL=postgres://user:password@localhost/qr_menu_app
export S3_ENDPOINT=http://localhost:9000
export BUCKET=qr-menu-photos
export AWS_ACCESS_KEY_ID=...
export AWS_SECRET_ACCESS_KEY=...
./target/release/qr-menu-app
```

## Deployment

The project ships with a multi-stage Dockerfile:

- **Builder stage** (`rust:1.88-slim-bookworm`): builds the release binary
  with dependency layer caching; uses `--locked` to enforce the committed
  `Cargo.lock`.
- **Runtime stage** (`debian:bookworm-slim`): copies only the binary; runs as
  a non-root system user (`appuser`, UID 1001). Container healthcheck polls
  `GET /health` every 30 s.

For production:
- Wire `DATABASE_URL` to a managed Postgres instance.
- Set `S3_ENDPOINT` to a **browser-reachable** HTTPS URL (e.g. your cloud
  object storage endpoint). Photo image URLs are constructed from this value,
  so it must be resolvable by end-user browsers — not just the app server.
- Set `BASE_URL` to the public HTTPS URL behind your reverse proxy.
- Configure the bucket for anonymous download (read-only public access) so
  unsigned photo URLs load without credentials.

**Rate limiter note:** The in-process rate limiter reads the socket peer IP.
Behind a reverse proxy (nginx, Caddy, etc.) all users will share the proxy's
single IP quota — 10 combined login/signup attempts per 15-minute window
across all users is an availability risk in that configuration.
X-Forwarded-For handling is deferred to a future hardening pass.

## CI

GitHub Actions runs on every push/PR to `main`:

1. `cargo build --locked`
2. `cargo clippy --locked -- -D warnings`
3. `cargo fmt -- --check`
4. `cargo test --locked`

Cargo registry and `target/` are cached keyed on `Cargo.lock`.
