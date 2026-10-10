FROM python:3.13-slim@sha256:70729b46c69b4f1e97c4822c1af3df53a1476cf5ddc6c087c0c10bc3a5678c2f

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1
WORKDIR /app
COPY requirements.txt .
RUN pip install --no-cache-dir --disable-pip-version-check -r requirements.txt \
    && useradd --system --uid 10001 --no-create-home app
COPY server.py .
USER 10001:10001
EXPOSE 8081
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8081/live/health', timeout=2)"
CMD ["python", "server.py"]
