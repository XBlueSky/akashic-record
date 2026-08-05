# syntax=docker/dockerfile:1
# Builder uses cargo-chef so the 22-crate workspace's dependency graph
# can be compiled as a cached layer without hand-maintained stub mains.
# Image tag matches rust-toolchain.toml (1.95.0) so rustup does not
# re-download a second toolchain inside the build.
FROM lukemathwalker/cargo-chef:latest-rust-1.95.0 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
# Optional cargo features. Default empty preserves the production
# build. docker-compose.test.yml passes "test-fixtures" so the test
# stack exposes the fixture-seed HTTP surface.
ARG CARGO_FEATURES=""

COPY --from=planner /app/recipe.json recipe.json
RUN if [ -n "$CARGO_FEATURES" ]; then \
        cargo chef cook --release --recipe-path recipe.json \
            -p akashic-server --features "$CARGO_FEATURES"; \
    else \
        cargo chef cook --release --recipe-path recipe.json \
            -p akashic-server; \
    fi

# C5 side-fix: AKASHIC_BUILD_COMMIT is consumed by
# crates/akashic-server/build.rs at compile time. Default 'unknown' so
# docker build still succeeds without the arg; compose / CI passes the
# real short hash via --build-arg. Declared here (after chef cook, not
# before) so BuildKit's ENV-folded cache key doesn't invalidate the
# expensive dependency-cook layer on every per-commit build — this ARG
# has no effect on `cargo chef cook` (which never reads it) and is only
# consumed by build.rs in the `cargo build` step below.
ARG AKASHIC_BUILD_COMMIT=unknown
ENV AKASHIC_BUILD_COMMIT=$AKASHIC_BUILD_COMMIT

COPY . .
RUN if [ -n "$CARGO_FEATURES" ]; then \
        cargo build --release -p akashic-server --features "$CARGO_FEATURES"; \
    else \
        cargo build --release -p akashic-server; \
    fi

# Deviation from task-2-brief.md (recorded per task instructions): the
# brief specified `debian:bookworm-slim` here, but
# lukemathwalker/cargo-chef:latest-rust-1.95.0 is currently built on
# Debian trixie (glibc 2.41). A bookworm-slim runtime (glibc 2.36) fails
# at container start with `GLIBC_2.39' not found — verified via the C4
# drill / test-stack boot during Task 2. Runtime bumped to
# debian:trixie-slim to match the builder's glibc; revisit if the chef
# image's base ever moves back to bookworm.
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# C4: non-root system user. UID/GID 10001 avoids host-UID collisions
# in the typical 1000-2000 interactive-user range.
RUN groupadd --system --gid 10001 akashic \
    && useradd --system --uid 10001 --gid 10001 \
        --home-dir /home/akashic --create-home \
        --shell /usr/sbin/nologin akashic

# Cargo bin target is `akashic-server` (workspace carve-out); the
# operational name everywhere (compose CMD, runbooks, drills) remains
# `akashic-record` — rename at install time.
COPY --from=builder /app/target/release/akashic-server /usr/local/bin/akashic-record

# Pre-create ingest scratch dir with correct ownership. Default
# INGEST_CLONE_DIR is /tmp/akashic-ingest; transient between restarts.
RUN mkdir -p /tmp/akashic-ingest \
    && chown akashic:akashic /tmp/akashic-ingest

# HF cache target if Candle is ever the embedding provider. Production
# uses OpenAI; HF_HOME is preserve-in-image-only — re-downloads on
# container recreate are acceptable.
ENV HF_HOME=/home/akashic/.cache/huggingface

USER akashic

EXPOSE 8080
EXPOSE 8081
CMD ["akashic-record"]
