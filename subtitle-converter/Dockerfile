# Native inference is built from an immutable, checksum-verified MNN source.
# No upstream mutable prebuilt archive or Python conversion step is used.
FROM scratch AS native-source-cache
FROM rust:1.99.0-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa AS native-tools
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl clang libclang-dev cmake pkg-config g++ make nasm \
    && rm -rf /var/lib/apt/lists/*
RUN --mount=type=bind,from=native-source-cache,target=/native-source-cache \
    if [ -f /native-source-cache/mnn-6b1db4c2eacf8193e6b7a1841acb6e31a1938e82.tar.gz ]; then \
      cp /native-source-cache/mnn-6b1db4c2eacf8193e6b7a1841acb6e31a1938e82.tar.gz /tmp/mnn.tar.gz; \
    else curl --fail --location --retry 3 \
      https://codeload.github.com/alibaba/MNN/tar.gz/6b1db4c2eacf8193e6b7a1841acb6e31a1938e82 \
      --output /tmp/mnn.tar.gz; fi \
    && echo 'b22142b69082b2720eb4a86598d4d189cd6f44aeca28fc8d7f23ca34345b81c0  /tmp/mnn.tar.gz' | sha256sum --check --strict \
    && mkdir /opt/mnn \
    && tar -xzf /tmp/mnn.tar.gz --strip-components=1 -C /opt/mnn \
    && rm /tmp/mnn.tar.gz
RUN apt-get update && apt-get install -y --no-install-recommends git \
    && rm -rf /var/lib/apt/lists/* \
    && rustup component add clippy
ENV MNN_SOURCE_DIR=/opt/mnn \
    CMAKE_BUILD_PARALLEL_LEVEL=2 \
    CARGO_BUILD_JOBS=1

FROM native-tools AS native-ocr-build
WORKDIR /ocr-build
COPY native-ocr/Cargo.toml native-ocr/Cargo.lock ./
COPY native-ocr/src/ ./src/
COPY native-ocr/models.sha256 ./models.sha256
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/ocr-build/target \
    cargo clippy --release --locked --all-targets -- -D warnings \
    && cargo test --release --locked \
    && cargo build --release --locked \
    && cp target/release/subforge-ocr /opt/subforge-ocr

FROM native-tools AS ocr-models
COPY native-ocr/models.sha256 /opt/ocr-models/models.sha256
RUN cd /opt/ocr-models \
    && while read -r checksum filename; do \
      [ -n "$filename" ] || continue; \
      curl --fail --location --retry 3 \
        "https://raw.githubusercontent.com/zibo-chen/ocr-rs/8ee8c7e606094951b7a5c072f44e76b8c5ab8a27/models/$filename" \
        --output "$filename"; \
    done < models.sha256 \
    && sha256sum --check --strict models.sha256 \
    && find . -type d -exec chmod 0555 {} + \
    && find . -type f -exec chmod 0444 {} +

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
    && cargo build --release --locked --features smoke-tests --bin subforge-smoke \
    && cp target/release/subforge /build/subforge \
    && cp target/release/subforge-worker /build/subforge-worker \
    && cp target/release/subforge-smoke /build/subforge-smoke

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS runtime

ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl ffmpeg libicu72 libfontconfig1 libgssapi-krb5-2 \
      fonts-dejavu-core iso-codes mkvtoolnix libstdc++6 libgomp1 \
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
COPY --from=native-ocr-build /opt/subforge-ocr /usr/local/bin/subforge-ocr
COPY --from=ocr-models /opt/ocr-models /opt/ocr-models
# Subtitle Edit's image bridge uses this executable name and JSON protocol.
# The compatibility executable is the same Rust binary, not PaddleOCR/Python.
USER root
RUN cp /usr/local/bin/subforge-ocr /usr/local/bin/paddleocr \
    && chmod 0555 /usr/local/bin/subforge-ocr /usr/local/bin/paddleocr
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
      org.opencontainers.image.version="0.4.4" \
      io.subforge.edition="cpu"

# Disposable fixture checks use a native Rust CLI in the test target only.
FROM runtime AS test
COPY --from=build /build/subforge-smoke /usr/local/bin/subforge-smoke

FROM runtime AS final
