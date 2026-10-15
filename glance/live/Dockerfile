FROM rust:1.90.0-slim-bookworm@sha256:64232e656c058f4468e8d024e990acff04f0fd5a5c0a88a574dc37773d7325c9 AS build
WORKDIR /build
ENV CARGO_BUILD_JOBS=1
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo test --all-targets --locked \
    && cargo build --release --locked \
    && cp target/release/glance-live /build/glance-live

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS runtime
COPY --from=build /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build /build/glance-live /usr/local/bin/glance-live
USER 10001:10001
EXPOSE 8081
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/glance-live", "--healthcheck"]
ENTRYPOINT ["/usr/local/bin/glance-live"]
