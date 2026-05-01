# ─────────────────────────────────────────────────────────────────────────────
# Stage 1: Build
#   Use the official Rust image with the musl target pre-installed.
#   The resulting binary is fully static — no glibc dependency.
# ─────────────────────────────────────────────────────────────────────────────
FROM rust:1-alpine AS builder

# Install musl toolchain + build essentials
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconf

# Use musl target for fully static binary
RUN rustup target add x86_64-unknown-linux-musl

WORKDIR /app

# Copy dependency manifests first — leverages Docker layer cache
COPY Cargo.toml Cargo.lock ./

# Dummy build to cache dependencies
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --target x86_64-unknown-linux-musl \
    && rm -rf src

# Now copy real source
COPY src ./src

# Force rebuild of our crate (touch main.rs to bust the cache for our crate only)
RUN touch src/main.rs \
    && cargo build --release --target x86_64-unknown-linux-musl

# ─────────────────────────────────────────────────────────────────────────────
# Stage 2: Minimal runtime image
#   Use scratch (empty) for a ~10 MB image.
#   If you need CA certs (for HTTPS egress), use alpine instead.
# ─────────────────────────────────────────────────────────────────────────────
FROM alpine:3.20

# CA certs needed for HTTPS requests (image download fallback)
RUN apk add --no-cache ca-certificates

WORKDIR /app

# Copy the static binary
COPY --from=builder /app/target/x86_64-unknown-linux-musl/release/server-rs ./server-rs

# Runtime data directories (SQLite files are mounted via Docker volumes)
RUN mkdir -p /data/images

# Expose the game server port
EXPOSE 3001

# Default environment — override in docker-compose
ENV PORT=3001 \
    ARCHIVE_DB_PATH=/data/archive.sqlite \
    APP_DB_PATH=/data/app.sqlite \
    IMAGE_CACHE_DIR=/data/images

ENTRYPOINT ["./server-rs"]
