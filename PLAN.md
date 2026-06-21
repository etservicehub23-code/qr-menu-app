# QR Menu App — Project Plan

## Core idea

Let restaurant owners quickly create and edit a digital menu, then generate a
QR code that customers scan to view it — no app install, fast updates, no
reprinting physical menus when prices/items change.

## Primary users

- **Restaurant owner/admin** — creates an account, builds/edits the menu
  (categories, items, prices, photos, availability toggle), gets a QR code
  tied to a stable public URL.
- **Customer** — scans the QR code at the table, views a fast, mobile-first
  menu page. No login required.

## Tech stack (pure Rust)

- **Backend/web framework**: Axum
- **Templating**: Askama (compile-time checked HTML templates) for
  server-rendered menu pages — keeps the customer-facing path simple and fast
  without a JS framework
- **Database**: SQLite via `sqlx` (simple ops, single binary, fine at
  restaurant scale; can migrate to Postgres later if needed)
- **QR generation**: `qrcode` crate, rendered to PNG/SVG and cached
- **Auth**: session-based auth for owners (e.g. `axum-login` or a minimal
  hand-rolled session+password-hash with `argon2`)
- **Image handling**: local filesystem storage for v1, optional S3-compatible
  storage later
- **Deployment**: single static binary + SQLite file, deployable on a small
  VPS or fly.io/Render

## Data model (v1)

- `restaurants` (id, owner_id, name, slug, theme/branding fields)
- `users` (id, email, password_hash)
- `menu_categories` (id, restaurant_id, name, sort_order)
- `menu_items` (id, category_id, name, description, price, photo_url,
  is_available, sort_order)
- QR code encodes `https://<domain>/m/<restaurant-slug>`

## Milestones

1. **Scaffold & data layer**
   - Axum app skeleton, SQLite schema + migrations (`sqlx migrate`)
   - Owner signup/login
2. **Menu CRUD (owner side)**
   - Create/edit/delete categories and items
   - Toggle item availability
   - Basic admin UI (server-rendered forms, no JS framework needed for v1)
3. **Public menu page (customer side)**
   - Mobile-first read-only menu view at `/m/<slug>`
   - Fast load, no auth, cache-friendly
4. **QR code generation**
   - Generate QR pointing at the public menu URL
   - Downloadable PNG/SVG for owners to print
5. **Polish**
   - Branding (logo, accent color) per restaurant
   - Item photos
   - Basic analytics (scan count, optional)
6. **Deployment**
   - Containerize, deploy single binary + SQLite (or swap to Postgres if
     multi-instance is needed)

## Open questions

- Multi-language menus needed?
- Do owners need multiple QR codes (e.g. per table) or one per restaurant?
- Payment/ordering integration in scope, or read-only menu only for v1?

## Status

v1 scaffold created (`cargo init`, pure Rust binary). No application code yet.
