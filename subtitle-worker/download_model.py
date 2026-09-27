"""Pre-download a Faster Whisper model into the worker's persistent cache."""

import os
import sys

from faster_whisper.utils import available_models, download_model


model = sys.argv[1] if len(sys.argv) > 1 else os.getenv("WHISPER_MODEL", "medium")
if model not in available_models():
    raise SystemExit(f"Unknown model {model!r}. Available: {', '.join(available_models())}")

cache_dir = os.getenv("HF_HUB_CACHE", "/models/hub")
print(f"Downloading {model} into {cache_dir}...", flush=True)
print(download_model(model, cache_dir=cache_dir), flush=True)
