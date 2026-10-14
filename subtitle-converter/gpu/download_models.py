#!/usr/local/bin/python3
"""Fetch and safely unpack the checksum-pinned official Paddle model set."""

from __future__ import annotations

import hashlib
import http.client
import os
import shutil
import tarfile
import tempfile
from urllib.parse import urlsplit
from pathlib import Path, PurePosixPath

BASE_URL = "https://paddle-model-ecology.bj.bcebos.com/paddlex/official_inference_model/paddle3.0.0/"
MAX_ARCHIVE_BYTES = 128 * 1024 * 1024
MAX_MODEL_BYTES = 512 * 1024 * 1024
MAX_MEMBERS = 500
CHUNK = 1024 * 1024


def parse_manifest(path: Path):
    entries = []
    for line in path.read_text(encoding="ascii").splitlines():
        fields = line.split()
        if not fields:
            continue
        if len(fields) != 2 or len(fields[0]) != 64 or not fields[1].endswith("_infer.tar"):
            raise ValueError("invalid model manifest")
        if any(c not in "0123456789abcdef" for c in fields[0]):
            raise ValueError("invalid model checksum")
        model = fields[1][:-len("_infer.tar")]
        if not model or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-" for c in model):
            raise ValueError("invalid model name")
        entries.append((fields[0], fields[1], model))
    if not entries or len({entry[1] for entry in entries}) != len(entries):
        raise ValueError("empty or duplicate model manifest")
    return entries


def fetch(url: str, destination: Path, expected_sha: str):
    parsed = urlsplit(url)
    trusted = urlsplit(BASE_URL)
    if parsed.scheme != "https" or parsed.netloc != trusted.netloc or parsed.query or parsed.fragment or not parsed.path.startswith(trusted.path):
        raise ValueError("model URL is outside the trusted HTTPS source")
    digest = hashlib.sha256()
    size = 0
    connection = http.client.HTTPSConnection(trusted.hostname, timeout=90)
    try:
        connection.request("GET", parsed.path, headers={"User-Agent": "homelab-subtitle-converter/0.3"})
        response = connection.getresponse()
        if response.status != 200:
            raise ValueError("model server rejected the download or attempted a redirect")
        with destination.open("xb") as output:
            while True:
                chunk = response.read(CHUNK)
                if not chunk:
                    break
                size += len(chunk)
                if size > MAX_ARCHIVE_BYTES:
                    raise ValueError("model archive exceeds size limit")
                digest.update(chunk)
                output.write(chunk)
    finally:
        connection.close()
    if digest.hexdigest() != expected_sha:
        raise ValueError("model archive checksum mismatch")


def extract(archive: Path, destination: Path, model: str):
    seen = set()
    unpacked = 0
    member_count = 0
    with tarfile.open(archive, mode="r:") as bundle:
        for member in bundle:
            member_count += 1
            if member_count > MAX_MEMBERS:
                raise ValueError("model archive contains an invalid number of entries")
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or not path.parts or path.parts[0] != f"{model}_infer":
                raise ValueError("model archive contains an unsafe path")
            relative_parts = path.parts[1:]
            if not relative_parts:
                if not member.isdir():
                    raise ValueError("model archive root is invalid")
                continue
            relative = Path(*relative_parts)
            target = destination / relative
            key = relative.as_posix()
            if key in seen:
                raise ValueError("model archive contains duplicate paths")
            seen.add(key)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            if not member.isfile() or member.size < 0:
                raise ValueError("model archive contains a link or special file")
            unpacked += member.size
            if unpacked > MAX_MODEL_BYTES:
                raise ValueError("unpacked model exceeds size limit")
            target.parent.mkdir(parents=True, exist_ok=True)
            source = bundle.extractfile(member)
            if source is None:
                raise ValueError("model archive entry is unreadable")
            with source, target.open("xb") as output:
                shutil.copyfileobj(source, output, CHUNK)
        if member_count == 0:
            raise ValueError("model archive is empty")
        if not (destination / "inference.json").is_file():
            raise ValueError("model archive is missing inference.json")


def verify_cached(archive: Path, expected_sha: str):
    if archive.is_symlink() or not archive.is_file() or archive.stat().st_size > MAX_ARCHIVE_BYTES:
        raise ValueError("cached model archive is invalid")
    digest = hashlib.sha256()
    with archive.open("rb") as source:
        for chunk in iter(lambda: source.read(CHUNK), b""):
            digest.update(chunk)
    if digest.hexdigest() != expected_sha:
        raise ValueError("model archive checksum mismatch")


def main():
    manifest = Path(os.environ.get("MODEL_MANIFEST", str(Path(tempfile.gettempdir()) / "models.sha256")))
    root = Path(os.environ.get("MODEL_ROOT", "/opt/paddle-models"))
    root.mkdir(parents=True, exist_ok=True)
    archive_cache = os.environ.get("MODEL_ARCHIVE_CACHE")
    with tempfile.TemporaryDirectory(prefix="paddle-models-") as temporary:
        cache = Path(temporary)
        for checksum, archive_name, model in parse_manifest(manifest):
            cached = Path(archive_cache) / archive_name if archive_cache else None
            downloaded = cache / archive_name
            if cached is not None and cached.exists():
                verify_cached(cached, checksum)
                downloaded = cached
            else:
                fetch(BASE_URL + archive_name, downloaded, checksum)
            target = root / model
            target.mkdir(mode=0o755)
            extract(downloaded, target, model)
            if downloaded.parent == cache:
                downloaded.unlink()
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception:
        raise SystemExit("model download or verification failed")
