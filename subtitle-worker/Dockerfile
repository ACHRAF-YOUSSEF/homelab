FROM rust:1.99.0-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa AS whisper-build
ARG WHISPER_CPP_COMMIT=2eeeba56e9edd762b4b38467bab96c2517163158
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates cmake g++ make git \
    && rm -rf /var/lib/apt/lists/* \
    && git clone https://github.com/ggml-org/whisper.cpp.git /src/whisper.cpp \
    && git -C /src/whisper.cpp checkout --detach "$WHISPER_CPP_COMMIT" \
    && cmake -S /src/whisper.cpp -B /src/whisper.cpp/build -DCMAKE_BUILD_TYPE=Release \
      -DBUILD_SHARED_LIBS=OFF -DGGML_BACKEND_DL=OFF -DGGML_NATIVE=OFF \
      -DWHISPER_BUILD_TESTS=OFF -DWHISPER_BUILD_EXAMPLES=ON -DWHISPER_BUILD_SERVER=OFF \
    && cmake --build /src/whisper.cpp/build --config Release --target whisper-cli -j2

FROM rust:1.99.0-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa AS app-build
RUN apt-get update && apt-get install -y --no-install-recommends build-essential \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build
ENV CARGO_BUILD_JOBS=2
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
COPY web/ ./web/
RUN cargo build --release --locked \
    && cargo build --release --locked --features fixture-probe --bin subtitle-studio-probe

FROM debian:bookworm-slim@sha256:7c7b2c966bc9ee8cedfeef67e0e279108992c77681fa595db4a9d65c06ccc587 AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates ffmpeg libgomp1 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 subtitle \
    && useradd --uid 1000 --gid 1000 --create-home subtitle \
    && mkdir -p /data /models /media/output /library \
    && chown -R subtitle:subtitle /data /models /media /library
COPY --from=app-build /build/target/release/subtitle-studio /usr/local/bin/subtitle-studio
COPY --from=whisper-build /src/whisper.cpp/build/bin/whisper-cli /usr/local/bin/whisper-cli
WORKDIR /app
ENV SUBTITLE_BIND_ADDRESS=0.0.0.0 SUBTITLE_PORT=8099 \
    SUBTITLE_LIBRARY_ROOT=/library SUBTITLE_OUTPUT_ROOT=/media/output \
    SUBTITLE_DATA_ROOT=/data WHISPER_MODEL_ROOT=/models
EXPOSE 8099
HEALTHCHECK --interval=30s --timeout=3s --retries=3 CMD ["/usr/local/bin/subtitle-studio", "--healthcheck"]
USER 1000:1000
ENTRYPOINT ["/usr/local/bin/subtitle-studio"]
CMD ["serve"]

FROM runtime AS test
COPY --from=app-build /build/target/release/subtitle-studio-probe /usr/local/bin/subtitle-studio-probe

FROM runtime AS final
