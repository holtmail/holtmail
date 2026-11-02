# syntax=docker/dockerfile:1
# Holtmail: one image with the `holtmail` binary and the built web app.
#   docker build -t holtmail .
#   docker run -p 8080:8080 -v holtmail-data:/data holtmail
ARG RUST_VERSION=1.99
ARG BUN_VERSION=1.4.4

FROM oven/bun:${BUN_VERSION} AS web
WORKDIR /app
COPY . .
RUN bun install --frozen-lockfile --ignore-scripts && bun run build

FROM rust:${RUST_VERSION}-trixie AS server
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked -p holtmail-server \
 && cp target/release/holtmail /usr/local/bin/holtmail

FROM debian:trixie-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data holtmail \
 && mkdir -p /data && chown 10001:10001 /data
COPY --from=server /usr/local/bin/holtmail /usr/local/bin/holtmail
COPY --from=web /app/apps/web/dist /app/web
ENV HOLTMAIL_DATA_DIR=/data \
    HOLTMAIL_LISTEN=0.0.0.0:8080 \
    HOLTMAIL_WEB_DIR=/app/web
USER 10001:10001
VOLUME ["/data"]
EXPOSE 8080
STOPSIGNAL SIGINT
ENTRYPOINT ["holtmail"]
CMD ["serve"]
