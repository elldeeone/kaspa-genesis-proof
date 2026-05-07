FROM rust:1.87-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY . .
RUN cargo build --release --bin web

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libstdc++6 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/web /usr/local/bin/kaspa-genesis-web
COPY static /app/static

ENV KASPA_PROOF_STATIC_DIR=/app/static
ENTRYPOINT ["/usr/local/bin/kaspa-genesis-web"]
