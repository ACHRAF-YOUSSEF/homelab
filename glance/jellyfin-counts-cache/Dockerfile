FROM rust:1.90.0-slim-bookworm@sha256:64232e656c058f4468e8d024e990acff04f0fd5a5c0a88a574dc37773d7325c9 AS build
WORKDIR /build
ENV CARGO_BUILD_JOBS=1
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target,sharing=locked \
    find src tests -type f -exec touch {} + \
    && cargo test --all-targets --locked \
    && cargo build --release --locked \
    && cp target/release/jellyfin-counts-cache /build/jellyfin-counts-cache

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587
COPY --from=build /build/jellyfin-counts-cache /usr/local/bin/jellyfin-counts-cache
USER 1000:1000
EXPOSE 8765
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/jellyfin-counts-cache", "--healthcheck"]
ENTRYPOINT ["/usr/local/bin/jellyfin-counts-cache"]
