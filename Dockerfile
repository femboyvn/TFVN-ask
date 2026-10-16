FROM rust:1.94-bookworm AS build

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim

# bookworm-slim has no CA bundle, and this network rejects plain HTTP to
# deb.debian.org. Seed apt from the build image, then install the real package.
COPY --from=build /etc/ssl/certs/ca-certificates.crt /tmp/ca-certificates.crt
RUN sed -i 's|http://deb.debian.org|https://deb.debian.org|g' /etc/apt/sources.list.d/debian.sources \
    && apt-get -o Acquire::https::CAInfo=/tmp/ca-certificates.crt update \
    && apt-get -o Acquire::https::CAInfo=/tmp/ca-certificates.crt install -y --no-install-recommends ca-certificates \
    && rm -f /tmp/ca-certificates.crt \
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
