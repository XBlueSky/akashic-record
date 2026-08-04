# syntax=docker/dockerfile:1
FROM rust:1.80 AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm AS runtime
WORKDIR /app
COPY --from=builder /app/target/release/server /usr/local/bin/server
CMD ["server"]
