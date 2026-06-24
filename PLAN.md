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
- 2026-06-21 (later): Milestone 1 step 1 implemented — minimal Axum+Tokio
  HTTP server skeleton with `GET /health` returning `ok`, binding
  `0.0.0.0:3000`. Verified with `cargo build` and a manual curl smoke test
  (200 ok). Commit `92e178f`, pushed to main.
  - Oracle verdict: **approved**, confidence high. "Code matches Axum's
    documented server pattern... remaining concerns are structural
    readiness issues rather than correctness failures." No blocking
    issues. Non-blocking suggestions for a later cleanup pass: extract an
    `app() -> Router` builder for testability, parse the bind address from
    an env var instead of hardcoding `0.0.0.0:3000`, have `main` return
    `Result` instead of `.expect()`-ing.
  - Next run: proceed to the next increment of Milestone 1 — Postgres
    schema + `sqlx` migrations (start with `users` and `restaurants`
    tables per the data model above). The bind-address/testability
    cleanups above can be folded into that step or a later one; they are
    not a prerequisite.
- 2026-06-22: Milestone 1 step 2 implemented — added `sqlx` (postgres,
  runtime-tokio, tls-rustls, migrate features), a migration creating
  `users` and `restaurants` tables per the data model above (BIGSERIAL
  PKs, `owner_id` FK, `slug` UNIQUE, index on `owner_id`), and wired
  Postgres connect + `sqlx::migrate!` on startup into `main.rs` via
  `DATABASE_URL`. Verified end-to-end against a throwaway local
  Postgres 16 docker container: migration applied cleanly, tables/
  columns/constraints inspected via `psql \d`, `/health` still
  responded `ok`. `cargo build` clean. Commit `f25d5fc`, pushed to
  main.
  - Oracle verdict: **flagged issues, not a clean approval** (high
    confidence). Step is "broadly acceptable as a Milestone 1 data-layer
    foundation, but should not be treated as fully locked in yet."
    Concrete concerns, ranked:
    1. `email TEXT UNIQUE` allows case-duplicate accounts
       (`Owner@x.com` vs `owner@x.com`) — fix with `CITEXT` or a unique
       index on `lower(email)`.
    2. `slug TEXT UNIQUE` has no DB-level format constraint — add a
       `CHECK` rejecting empty/uppercase/whitespace/slash/weird-unicode
       slugs (oracle suggested `CHECK (slug ~ '^[a-z0-9][a-z0-9-]{1,62}[a-z0-9]$')`).
    3. `restaurants.name` has no non-empty constraint — add
       `CHECK (length(trim(name)) > 0)`.
    4. `updated_at` is not auto-maintained — not blocking yet, but note
       this is manual-update-only for now (no trigger).
    5. `owner_id` FK has unspecified delete behavior (`ON DELETE NO
       ACTION` by default) — should be a deliberate choice later, not
       blocking now.
    6. `BIGSERIAL` is acceptable but the oracle prefers
       `BIGINT GENERATED BY DEFAULT AS IDENTITY` as the more modern
       Postgres form — optional, not blocking by itself, only relevant
       if combined with the migration rewrite below.
    7. Startup migration-on-boot with `.expect()` panics is fine for
       single-instance local/dev now, but should be explicitly treated
       as dev-only wiring — split migrations from app boot before
       deployment/multi-instance testing (Milestone 7 concern, not now).
    Oracle's suggested replacement migration (citext extension, `CHECK`
    constraints on slug/name, `GENERATED BY DEFAULT AS IDENTITY` PKs) is
    recorded for the next run to apply. Oracle also gave a minimal
    negative-test validation plan: migration applies on empty DB,
    duplicate case-insensitive emails rejected, invalid/duplicate slugs
    rejected, restaurant insert with nonexistent `owner_id` rejected,
    `/health` still works after startup migration.
  - Per the working agreement, this counts as a flagged step: **do not
    proceed to the next milestone (owner signup/login) yet.** Next run
    must: rewrite `migrations/20260622000000_create_users_and_restaurants.sql`
    (or add a follow-up migration, project's choice at that time) to
    add `citext` email uniqueness and the slug/name `CHECK` constraints
    per the oracle's suggested SQL above, re-verify against a throwaway
    Postgres container including the negative-test cases the oracle
    listed, then run exactly one codex-oracle prompt asking specifically
    whether the revision resolves the flagged concerns. Only once that
    comes back approved should a subsequent run move on to Milestone 1's
    remaining piece (owner signup/login: argon2, sessions, CSRF, rate
    limiting).
- 2026-06-22 (later): Milestone 1 step 2 revision — per the working
  agreement, rewrote `migrations/20260622000000_create_users_and_restaurants.sql`
  (not yet applied to any persistent/shared DB, so safe to rewrite
  in place rather than add a follow-up migration) to address the
  prior flagged concerns: `CREATE EXTENSION IF NOT EXISTS citext` +
  `email CITEXT NOT NULL UNIQUE` for case-insensitive uniqueness,
  `restaurants.name CHECK (length(trim(name)) > 0)`,
  `restaurants.slug CHECK (slug ~ '^[a-z0-9][a-z0-9-]{1,62}[a-z0-9]$')`,
  and `BIGINT GENERATED BY DEFAULT AS IDENTITY` PKs instead of
  `BIGSERIAL`. `cargo build` clean. Re-verified against a fresh
  throwaway Postgres 16 docker container: schema applies cleanly,
  and all 8 of the oracle's suggested negative tests behave
  correctly (case-duplicate email rejected, uppercase slug rejected,
  short/trailing-dash slug rejected, blank/whitespace name rejected,
  duplicate slug rejected, nonexistent `owner_id` FK rejected, plus
  happy-path inserts and `/health` working). Commit `e7d3fae`, pushed
  to main.
  - Oracle verdict: **approved**, confidence high. "This revised
    migration resolves the prior blocking schema concerns... It is
    now safe for the next cron run to proceed to the next Milestone 1
    increment: owner signup/login with argon2, sessions, CSRF, and
    rate limiting." Non-blocking items explicitly deferred (not
    prerequisites): `updated_at` is still manual-only (add a trigger
    before restaurant edit/CRUD handlers exist), `owner_id` FK still
    defaults to `ON DELETE NO ACTION` (decide deliberately before
    account-deletion exists), `citext` extension creation should be
    verified against the least-privileged DB role before managed
    deployment, and the slug pattern doesn't reserve words like
    `admin`/`login`/`health` (not urgent while routes are `/m/<slug>`
    only). Oracle also flagged that rewriting an already-applied
    migration is only safe pre-production — confirmed true here (no
    shared DB exists yet); once a real environment runs this
    migration, use a follow-up migration instead of editing history.
  - Next run: proceed to the remaining piece of Milestone 1 — owner
    signup/login. Per PLAN.md's tech stack and the oracle's
    suggestions: argon2 for password hashing, `tower-sessions` or
    `axum-login` for sessions (no hand-rolled session crypto), CSRF
    protection on the auth forms, session rotation on login and
    invalidation on logout, and basic rate limiting on login/signup.
    Keep this to the smallest reasonable first slice (e.g. signup +
    login with argon2 + sessions) rather than building all of CSRF +
    rate limiting + rotation in one step if it doesn't fit a single
    small increment — split further increments across runs as needed.
- 2026-06-23: Milestone 1 step 3 implemented — owner signup/login slice.
  Added `src/auth.rs` with signup/login/logout/index handlers, wired
  into `main.rs` via `tower-sessions` + `tower-sessions-sqlx-store`
  (Postgres-backed sessions, no local-disk state). Argon2id password
  hashing on signup; session ID rotated with `cycle_id()` on
  login/signup (session-fixation prevention); session `flush()`'d on
  logout. Hit and fixed a real dependency conflict: latest
  `tower-sessions` (0.15.0) depends on `tower-sessions-core 0.15`,
  but the latest `tower-sessions-sqlx-store` (0.15.0) only supports
  `tower-sessions-core 0.14` — pinned `tower-sessions = "=0.14.0"` to
  match. Also hit and fixed a citext gotcha: `email = $1` compared
  case-sensitively because sqlx binds the parameter as an explicit
  `text` type, so Postgres resolved `citext = text` by casting the
  column down to `text`; fixed with an explicit `$1::citext` cast on
  every bound email comparison. `cargo build` and `cargo clippy`
  clean. Verified end-to-end against a throwaway Postgres 16 docker
  container: signup, login (including case-insensitive email),
  wrong-password rejection, duplicate-email rejection (409),
  short-password/blank-email rejection (400), logout, and re-login
  all behaved correctly. CSRF protection and rate limiting were
  deliberately deferred to a follow-up increment per the working
  agreement (kept this slice to signup + login + argon2 + sessions).
  Commit `69fb94e`, pushed to main.
  - Oracle verdict: **flagged issues, not a clean approval** (high
    confidence). Core mechanics judged sound (bound SQL params, Argon2
    usage, session rotation/flush, the `$1::citext` fix). Concrete
    issues, ranked:
    1. **Concrete bug**: the logout link in `src/auth.rs` (`index`
       handler) renders `<a href="/logout">Log out</a>` (a GET), but
       the `/logout` route in `main.rs` only accepts POST — clicking
       the link will 405, logout is effectively broken from the UI.
    2. No rate limiting on `/login`/`/signup` — online password
       guessing and cheap Argon2-hash DoS are both possible right now.
       Oracle: acceptable to defer only as long as this isn't exposed
       publicly yet (it isn't).
    3. CSRF still missing — oracle agrees deferring past this slice is
       reasonable for now, but says it must land before any
       authenticated state-changing owner workflow (i.e. before
       Milestone 2's menu CRUD forms).
    4. Session cookie config is all implicit defaults — no explicit
       idle/absolute expiry is set (defaults to `None`, i.e.
       session-only cookie with no server-side expiry), worth setting
       explicitly later.
    5. Minor error-handling info leakage: signup's 409 on duplicate
       email enables email enumeration; the "corrupt password hash"
       500 message exposes an internal condition. Not blocking, but
       worth tightening.
    6. Cosmetic: prefer `tower-sessions = "0.14"` over the exact-pin
       `"=0.14.0"` so patch releases aren't blocked (the mismatch is
       at the `tower-sessions-core` major/minor level, not patch).
    Oracle confirmed the `tower-sessions-sqlx-store` pin/workaround is
    the right call (no stronger Postgres-backed alternative currently
    on crates.io for this milestone) and confirmed the `::citext` cast
    pattern is correct and must be applied to every future bound
    comparison against `users.email`.
  - Per the working agreement, this counts as a flagged step: **do not
    proceed to Milestone 2 (menu CRUD) or any new feature work yet.**
    Next run must, as a single small revision (not new milestone
    work): (a) fix the logout link/form to actually POST (e.g. a tiny
    `<form method="post" action="/logout">` button instead of an `<a>`
    link), (b) relax the `tower-sessions` pin from `"=0.14.0"` to
    `"0.14"`, (c) optionally tighten the two info-leak messages if it
    fits in the same small step. Rate limiting, CSRF, and explicit
    session expiry remain deferred (not required to clear this flag)
    but must land before Milestone 2's menu CRUD forms per the
    oracle's CSRF note above. After making the fix, re-verify the
    logout flow against a throwaway Postgres container (POST clears
    the cookie/session, subsequent `/` shows logged-out), then run
    exactly one codex-oracle prompt asking specifically whether the
    revision resolves the flagged logout bug. Only once that comes
    back approved should a subsequent run move on to CSRF/rate
    limiting or Milestone 2.
- 2026-06-23 (later): Logout GET/POST bug fix — per the working
  agreement, revised `src/auth.rs`'s `index` handler to render logout
  as a `<form method="post" action="/logout">` button instead of the
  GET `<a href="/logout">` link that the oracle flagged. Also relaxed
  `Cargo.toml`'s `tower-sessions` pin from the exact-pin `"=0.14.0"`
  to `"0.14"` per the oracle's prior cosmetic suggestion (Cargo's
  caret requirement keeps it within `>=0.14.0, <0.15.0`, so it still
  can't jump to the incompatible `0.15` line). `cargo build` and
  `cargo clippy` clean. Re-verified against a throwaway Postgres 16
  docker container: authenticated `/` now shows a working logout
  button, `POST /logout` clears the session and redirects to
  `/login`, `GET /logout` correctly 405s. Commit `ce8203b`, pushed to
  main.
  - Oracle verdict: **resolved / approved** (high confidence). "Yes,
    this resolves the flagged logout GET/POST bug." Confirmed
    `/logout` is POST-only and the rendered form now matches it,
    confirmed the `tower-sessions = "0.14"` relaxation is safe (caret
    requirement can't cross into 0.15). One minor non-blocking nit
    raised: the `<form>` is nested inside a `<p>` tag, which is
    invalid HTML (browsers auto-repair it, but it's worth a small
    markup cleanup — wrap in a `<div>`/sibling `<p>` instead of
    nesting). Oracle also suggested (not blocking) adding a small
    regression test asserting `GET /logout` is 405, the authenticated
    index contains a POST logout form, and `POST /logout` clears the
    session — and reiterated that logout being POST-only is not itself
    a CSRF defense (CSRF protection is still a prerequisite before
    Milestone 2's menu CRUD forms, as previously noted, not before
    this fix).
  - Next run: the originally flagged step is now resolved — clear to
    proceed with new work. Per the prior oracle review's ordering,
    prioritize CSRF protection on the auth forms (signup/login/logout)
    before starting Milestone 2's menu CRUD work, since CSRF must land
    before any authenticated state-changing owner workflow. Basic
    rate limiting on `/login`/`/signup` and the `<p>`-nesting markup
    cleanup remain valid small follow-ups, but are not blocking and
    can be folded into the CSRF step or split into their own run if
    CSRF alone is enough for one increment.
- 2026-06-24: Milestone 1 step 4 — CSRF protection on auth forms. Added
  the synchronizer token pattern to `src/auth.rs` using no new
  dependencies: `new_csrf_token()` generates 32-byte `OsRng` entropy,
  hex-encodes it, stores it in the session under key `"csrf_token"`,
  and returns the token to embed as `<input type="hidden"
  name="authenticity_token">` in each form. `verify_csrf_token()` reads
  the stored token, removes it (single-use: remove before compare so an
  error still invalidates it), then compares. All three POST handlers
  (signup, login, logout) call `verify_csrf_token` first. GET handlers
  (`signup_form`, `login_form`, authenticated `index`) generate and embed
  the token. The `logout` handler now accepts `Form<LogoutForm>` with
  `authenticity_token`. Also tightened the "corrupt password hash" 500
  message to "failed to verify password" (minor info-leak fix). `cargo
  build` clean. Commit `190c6b7`, pushed to main.
  - Oracle verdict: **flagged / not a clean approval** (confidence high).
    "Basically sound for current auth forms, but I would not treat this
    as cleanly unblocking Milestone 2 menu CRUD unchanged, because the
    single global single-use token will be brittle once multiple admin
    forms/pages exist. Fix the token model before adding CRUD forms."
    Concrete concerns, ranked:
    1. **Multi-tab / multi-form invalidation (primary blocker for M2)**:
       `signup_form`, `login_form`, and `index` all overwrite the same
       `"csrf_token"` key. Opening a second form replaces the first
       token, making the first form fail. Acceptable for today's tiny
       auth-only surface; not acceptable for Milestone 2 which will have
       multiple admin forms.
    2. **"Single-use" guarantee is weaker than it looks**: `remove()`
       operates on the request-local session copy; `tower-sessions`
       only persists modified sessions on non-5xx responses. If
       `verify_csrf_token` succeeds but a later handler in the same
       request returns 500, the removal may not persist to the store
       (leaving the token reusable).
    3. **Concurrent replay**: two simultaneous POSTs with the same valid
       token can both load the old session record, both compare
       successfully, and both proceed. Not a CSRF break but not strong
       replay prevention.
    4. **Remove-before-compare token-depletion DoS**: a cross-site
       attacker submitting an invalid token consumes the victim's current
       token, forcing the victim to reload the form. Preferable to CSRF
       but unnecessary friction if replay prevention isn't needed.
    5. **Non-constant-time comparison** (`stored != submitted`):
       acceptable in practice given 256-bit random token hidden by SOP
       and consumed on failure, but worth noting.
    6. **Local HTTP dev with `Secure=true` default**: curl smoke tests
       work; browsers will not send the session cookie over plain HTTP.
    Oracle's recommended fix for items 1–2: replace the single
    `"csrf_token": String` with a bounded token pool —
    `"csrf_tokens": Vec<String>`, capped at ~16–32 entries. `new_csrf_token`
    appends to the pool. `verify_csrf_token` scans for the matching
    token, removes only that entry, and (to address concern #2) calls
    `session.save().await` explicitly after removal. This resolves the
    two-tab problem and makes single-use persistence more reliable.
    Alternative (simpler, acceptable if replay prevention isn't needed):
    one stable per-session token rotated only on login/logout.
  - Per the working agreement, this counts as a flagged step: **do not
    proceed to Milestone 2 (menu CRUD) yet.** Next run must: upgrade
    the CSRF token storage from a single string to a bounded
    `Vec<String>` pool under key `"csrf_tokens"` per the oracle's
    guidance above — `new_csrf_token` appends (capped at 16 entries),
    `verify_csrf_token` scans-and-removes only the matched token, then
    calls `session.save()` explicitly. `cargo build` must be clean.
    Run exactly one codex-oracle prompt asking whether the revised token
    pool approach resolves the flagged multi-tab and single-use
    persistence concerns. Only once that comes back approved should a
    subsequent run proceed to Milestone 2's menu CRUD work.
