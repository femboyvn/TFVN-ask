FROM rust:1.94-bookworm AS build

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --no-create-home knowsphere \
    && mkdir /data \
    && chown knowsphere:knowsphere /data

COPY --from=build /app/target/release/knowsphere /usr/local/bin/knowsphere

ENV CONVERSATION_STORE_PATH=/data/conversations.json \
    MEMORY_STORE_PATH=/data/memories.json
WORKDIR /data
USER knowsphere
VOLUME ["/data"]

ENTRYPOINT ["/usr/local/bin/knowsphere"]
