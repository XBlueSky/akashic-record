FROM rust:1.88 AS builder

# C5 side-fix: AKASHIC_BUILD_COMMIT is consumed by build.rs at compile
# time. Default 'unknown' so docker build still succeeds without the
# arg; compose / CI passes the real short hash via --build-arg.
ARG AKASHIC_BUILD_COMMIT=unknown
ENV AKASHIC_BUILD_COMMIT=$AKASHIC_BUILD_COMMIT

# Optional cargo features. Default empty preserves the production
# build. docker-compose.test.yml passes "test-fixtures" so the test
# stack exposes the fixture-seed HTTP surface.
ARG CARGO_FEATURES=""

WORKDIR /app
# Pre-fetch dependencies via the stub-main trick for layer cache reuse.
# build.rs is included so a build-script change invalidates the dep
# cache correctly.
COPY Cargo.toml Cargo.lock build.rs ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && if [ -n "$CARGO_FEATURES" ]; then \
           cargo build --release --features "$CARGO_FEATURES"; \
       else \
           cargo build --release; \
       fi \
    && rm -rf src

COPY src ./src
RUN touch src/main.rs \
    && if [ -n "$CARGO_FEATURES" ]; then \
           cargo build --release --features "$CARGO_FEATURES"; \
       else \
           cargo build --release; \
       fi

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# C4: non-root system user. UID/GID 10001 avoids host-UID collisions
# in the typical 1000-2000 interactive-user range.
RUN groupadd --system --gid 10001 akashic \
    && useradd --system --uid 10001 --gid 10001 \
        --home-dir /home/akashic --create-home \
        --shell /usr/sbin/nologin akashic

COPY --from=builder /app/target/release/akashic-record /usr/local/bin/akashic-record

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
