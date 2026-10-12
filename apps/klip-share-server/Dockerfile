# Scout's share server (M11.7). Build from the repository root:
#   docker build -f apps/klip-share-server/Dockerfile -t scout-share .
# Run (the address people reach it at is required):
#   docker run -d -p 127.0.0.1:8080:8080 -v scout-share-data:/data \
#     -e SCOUT_SHARE_PUBLIC_URL=https://clips.example.com scout-share
#   docker exec <container> scout-share token add my-pc

FROM rust:1.99-slim-trixie AS build
WORKDIR /src
COPY . .
# The image's Rust is the version the repo pins; skip rustup's toolchain file (it would
# download the same version again with clippy and rustfmt).
RUN rm -f rust-toolchain.toml \
 && CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=true \
    cargo build --release --locked -p klip-share-server --bin scout-share \
 && cp target/release/scout-share /scout-share

FROM debian:trixie-slim
RUN useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin scout \
 && mkdir -p /data && chown scout:scout /data
COPY --from=build /scout-share /usr/local/bin/scout-share
USER scout
ENV SCOUT_SHARE_DATA_DIR=/data \
    SCOUT_SHARE_LISTEN=0.0.0.0:8080
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
  CMD ["scout-share", "health"]
ENTRYPOINT ["scout-share"]
CMD ["serve"]
