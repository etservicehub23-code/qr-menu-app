# QR Menu App — Project Plan

## Core idea

Let restaurant owners quickly create and edit a digital menu, then generate a
QR code that customers scan to view it — no app install, fast updates, no
reprinting physical menus when prices/items change.

## Product shape (decided 2026-06-21)

This is a **multi-tenant SaaS**: any restaurant can sign up and use the
service. Many restaurants share the same running instance(s). This decision
(per Codex-oracle review) drives the infra choices below — SQLite + local
files is rejected; the stack now assumes Postgres + object storage from day
one rather than deferring that migration.

## Primary users

- **Restaurant owner/admin** — signs up, creates a restaurant, builds/edits
  the menu (categories, items, prices, photos, availability toggle), gets a
  QR code tied to a stable public URL.
- **Customer** — scans the QR code at the table, views a fast, mobile-first
  menu page. No login required.

## Tech stack (pure Rust)

- **Backend/web framework**: Axum
- **Templating**: Askama (compile-time checked HTML templates) for
  server-rendered menu pages — keeps the customer-facing path simple and fast
  without a JS framework
- **Database**: **Postgres** via `sqlx`, from day one (multi-tenant —
  enforce `restaurant_id` scoping on every query; foreign keys, unique
  constraints, timestamps mandatory)
- **QR generation**: `qrcode` crate; generate SVG/PNG on demand from the
  canonical public URL, no caching needed initially
- **Auth**: `tower-sessions` or `axum-login` + `argon2` for password
  hashing; secure cookies, CSRF protection on all admin forms, session
  rotation on login, logout invalidation, basic rate limiting on
  login/signup. No hand-rolled session crypto.
- **Image handling**: S3-compatible object storage from day one (not
  deferred); enforce file size limits, MIME validation, and resizing before
  storage
- **Deployment**: containerized Axum binary + managed Postgres + S3-compatible
  bucket; horizontally scalable since state lives in Postgres/object storage,
  not on local disk

## Data model (v1)

- `restaurants` (id, owner_id, name, slug UNIQUE, theme/branding fields,
  is_published, created_at, updated_at)
- `users` (id, email UNIQUE, password_hash, created_at)
- `menu_categories` (id, restaurant_id FK, name, sort_order)
- `menu_items` (id, category_id FK, name, description, price_cents INTEGER,
  currency, photo_url, is_available, sort_order, created_at, updated_at)
- QR code encodes `https://<domain>/m/<restaurant-slug>`
- All tenant-scoped queries must filter by `restaurant_id`/`owner_id` —
  enforced at the query layer, not just the application layer, wherever
  practical (e.g. row-level checks in handlers, integration tests asserting
  cross-tenant access is rejected)

## Milestones

1. **Scaffold & data layer**
   - Axum app skeleton, Postgres schema + migrations (`sqlx migrate`)
   - Owner signup/login (argon2, sessions, CSRF, rate limiting)
2. **Menu CRUD (owner side)**
   - Create/edit/delete categories and items, scoped to the authenticated
     owner's restaurant(s)
   - Toggle item availability
   - Basic admin UI (server-rendered forms, no JS framework needed for v1)
3. **Public menu page (customer side)**
   - Mobile-first read-only menu view at `/m/<slug>`
   - Fast load, no auth, cache-friendly
4. **QR code generation**
   - Generate QR pointing at the public menu URL
   - Downloadable PNG/SVG for owners to print
5. **Image uploads**
   - Owner uploads item/restaurant photos to S3-compatible storage
   - Size limits, MIME validation, resizing pipeline
6. **Polish**
   - Branding (logo, accent color) per restaurant
   - Basic analytics (scan count) — sampled/buffered, not a write per request
7. **Deployment**
   - Containerize, deploy against managed Postgres + S3-compatible bucket
   - Horizontal scaling validated (multiple app instances, shared state in
     Postgres/object storage only)

## Open questions

- Multi-language menus needed?
- Do owners need multiple QR codes (e.g. per table) or one per restaurant?
- Payment/ordering integration in scope, or read-only menu only for v1?

## Working agreement for autonomous development (cron-driven)

- Development proceeds in small steps against the milestones above, driven
  by a scheduled job (see cron setup).
- Every implementation step must be followed by a Codex-oracle review of the
  diff/step before moving on to the next step.
- If the oracle flags the step as wrong or risky, the next action is to
  draft a revised plan (update this file) and get that revision approved by
  the oracle before further implementation continues — do not keep building
  on a step the oracle rejected.

## Status

- 2026-06-21: Architecture decided — multi-tenant SaaS (Postgres + S3),
  initial Codex-oracle review done (see PR/commit history for full
  feedback). `cargo init` scaffold exists. No application code yet.
