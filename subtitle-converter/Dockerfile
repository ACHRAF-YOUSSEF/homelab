FROM python:3.12-slim-bookworm@sha256:34386ef0cb081344d7ec1c103ba398e6e9f64e9ab3a1509accc92a4e24a07258

ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl ffmpeg libicu72 libfontconfig1 libgssapi-krb5-2 \
      fonts-dejavu-core iso-codes mkvtoolnix tesseract-ocr tesseract-ocr-all \
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
COPY converter.py server.py integrations.py http_client.py ./
COPY web/ ./web/
ENV PYTHONUNBUFFERED=1 PYTHONDONTWRITEBYTECODE=1 \
    DOTNET_BUNDLE_EXTRACT_BASE_DIR=/tmp/dotnet \
    OMP_THREAD_LIMIT=2
USER 1000:1000
EXPOSE 8100
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8100/health', timeout=3)" || exit 1
# Container listener; Compose publishes only the configured host interface.
CMD ["python", "server.py", "--host", "0.0.0.0"]
