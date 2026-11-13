# QR Menu App

A multi-tenant SaaS that lets restaurant owners create and manage a digital
menu accessible via QR code — no app install required for customers, no
reprinting when prices change.

## Stack

- **Language**: Pure Rust (edition 2024)
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
- Rust toolchain (stable, edition 2024, >= 1.88)

### Quick start

```bash
cp .env.example .env
# Edit .env if needed (defaults work with the compose stack as-is)
docker compose up --build
```

The app binds to `http://localhost:3000`.

**MinIO setup (required before photo uploads work):**

1. Open the MinIO console at `http://localhost:9001`
   (credentials: `minioadmin` / `minioadmin`).
2. Create a bucket named `qr-menu-photos`.
3. Set the bucket's access policy to **Public** (anonymous read) so photo
   image URLs load in the browser without credentials.

**Photo image URLs — hostname note:** Photo URLs are constructed as
`S3_ENDPOINT/BUCKET/filename` (e.g. `http://minio:9000/qr-menu-photos/...`).
Port 9000 is exposed to the host by docker-compose, but the hostname `minio`
only resolves inside Docker by default. To make photo images load in your
browser, add the following line to your `/etc/hosts`:

```
127.0.0.1 minio
```

This lets the browser resolve `minio` to localhost where port 9000 is mapped.

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

On first startup, sqlx applies migrations automatically. The DB role must have
`CREATE` privileges and permission to install the `citext` extension
(`SUPERUSER` or `CREATE EXTENSION` privilege in Postgres 15+).

```bash
cargo build --release
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

For production, wire `DATABASE_URL` to a managed Postgres instance and
`S3_ENDPOINT`/`BUCKET` to your object storage provider. Set `BASE_URL` to
the public HTTPS URL behind your reverse proxy.

**Rate limiter note:** The in-process rate limiter reads the socket peer IP.
Behind a reverse proxy (nginx, Caddy, etc.) all users will share the proxy's
single IP quota. X-Forwarded-For handling is deferred to a future hardening
pass; until then, the rate limiter is not effective in proxied deployments.

## CI

GitHub Actions runs on every push/PR to `main`:

1. `cargo build --locked`
2. `cargo clippy --locked -- -D warnings`
3. `cargo fmt -- --check`
4. `cargo test --locked`

Cargo registry and `target/` are cached keyed on `Cargo.lock`.
