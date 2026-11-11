# Stage 1: build
FROM rust:1.86-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependency compilation separately from source
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release \
    && rm -rf src

# Build the real application
COPY src ./src
COPY templates ./templates
COPY migrations ./migrations
# Touch main.rs so cargo detects the change and rebuilds
RUN touch src/main.rs && cargo build --release

# Stage 2: minimal runtime image
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -u 1001 -g root appuser

WORKDIR /app

COPY --from=builder /app/target/release/qr-menu-app ./qr-menu-app

USER appuser

EXPOSE 3000

ENTRYPOINT ["./qr-menu-app"]
