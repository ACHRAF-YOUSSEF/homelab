FROM rust:1.99.0-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa AS build
WORKDIR /build
ENV CARGO_BUILD_JOBS=1
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY web/ ./web/
COPY tests/ ./tests/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo test --locked \
    && cargo build --release --locked \
    && cp target/release/subforge /build/subforge \
    && cp target/release/subforge-worker /build/subforge-worker

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS runtime

ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl ffmpeg libicu72 libfontconfig1 libgssapi-krb5-2 \
      fonts-dejavu-core iso-codes mkvtoolnix tesseract-ocr tesseract-ocr-all \
      procps \
    && rm -rf /var/lib/apt/lists/*

# Official, self-contained Subtitle Edit CLI; verify each architecture's asset.
RUN case "$TARGETARCH" in \
      amd64) asset=x64; checksum=54cc660132e6fed2c24eeca8e869a65b99ff5d3195ac34a962b7e749f1dede7a ;; \
      arm64) asset=ARM64; checksum=6316965ccbf9378468e939749ace34c7c8e1dabdfeede0d8f1c28be46b1674e5 ;; \
      *) echo "Supported architectures: amd64 and arm64" >&2; exit 1 ;; \
    esac \
    && curl --fail --location --retry 3 \
      "https://github.com/SubtitleEdit/subtitleedit/releases/download/v5.2.0/SeConv-Linux-${asset}.tar.gz" \
      --output /tmp/seconv.tar.gz \
    && echo "$checksum  /tmp/seconv.tar.gz" | sha256sum --check --strict \
    && mkdir /opt/seconv \
    && tar -xzf /tmp/seconv.tar.gz -C /opt/seconv \
    && rm /tmp/seconv.tar.gz \
    && chmod +x /opt/seconv/seconv \
    && ln -s /opt/seconv/seconv /usr/local/bin/seconv \
    && seconv --version

RUN groupadd --gid 1000 converter \
    && useradd --uid 1000 --gid 1000 --create-home converter \
    && mkdir /data \
    && chown converter:converter /data
WORKDIR /app
COPY --from=build /build/subforge /usr/local/bin/subforge
COPY --from=build /build/subforge-worker /usr/local/bin/subforge-worker
ENV DOTNET_BUNDLE_EXTRACT_BASE_DIR=/tmp/dotnet \
    OMP_THREAD_LIMIT=1 \
    CONVERTER_ENABLE_OCR=false
USER 1000:1000
EXPOSE 8100
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD ["/usr/local/bin/subforge", "healthcheck"]
# Container listener; Compose publishes only the configured host interface.
CMD ["/usr/local/bin/subforge", "--host", "0.0.0.0"]
LABEL org.opencontainers.image.title="Subforge" \
      org.opencontainers.image.version="0.4.3" \
      io.subforge.edition="cpu"

# Python is a test harness dependency only, excluded from the final app image.
FROM runtime AS test
USER root
RUN apt-get update && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*
COPY tests/ /tests/
USER 1000:1000

FROM runtime AS final
