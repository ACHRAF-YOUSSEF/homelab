FROM nvidia/cuda:12.3.2-cudnn9-runtime-ubuntu22.04

RUN apt-get update && apt-get install -y --no-install-recommends ffmpeg python3 python3-pip python-is-python3 \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 1000 subtitle \
    && useradd --uid 1000 --gid 1000 --create-home subtitle \
    && mkdir -p /data /models/hub /media/output /library \
    && chown -R subtitle:subtitle /data /models /media /library

WORKDIR /app
COPY requirements.txt .
RUN python3 -m pip install --no-cache-dir --upgrade pip \
    && python3 -m pip install --no-cache-dir -r requirements.txt
COPY worker.py download_model.py validate_srt.py http_client.py ./
COPY web ./web

ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    HF_HOME=/models \
    HF_HUB_CACHE=/models/hub
EXPOSE 8099
USER 1000:1000
CMD ["python3", "worker.py"]
