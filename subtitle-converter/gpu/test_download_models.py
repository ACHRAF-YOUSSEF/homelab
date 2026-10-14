"""Offline archive validation tests for the pinned model downloader."""
from __future__ import annotations

import importlib.machinery
import importlib.util
import hashlib
import io
from pathlib import Path
import sys
import tarfile
import tempfile
import unittest
from unittest import mock


GPU_DIR = Path(__file__).resolve().parent
_loader = importlib.machinery.SourceFileLoader("subtitle_download_models", str(GPU_DIR / "download_models.py"))
_spec = importlib.util.spec_from_loader(_loader.name, _loader)
download_models = importlib.util.module_from_spec(_spec)
sys.modules[_loader.name] = download_models
_loader.exec_module(download_models)


def regular(name: str, data: bytes) -> tuple[tarfile.TarInfo, bytes]:
    info = tarfile.TarInfo(name)
    info.size = len(data)
    return info, data


class FakeResponse:
    def __init__(self, payload: bytes, status: int = 200):
        self.payload = payload
        self.status = status
        self.offset = 0

    def read(self, size: int) -> bytes:
        result = self.payload[self.offset:self.offset + size]
        self.offset += len(result)
        return result


class FakeHTTPSConnection:
    def __init__(self, response: FakeResponse):
        self.response = response
        self.requests = []
        self.closed = False

    def request(self, method: str, path: str, headers: dict):
        self.requests.append((method, path, headers))

    def getresponse(self):
        return self.response

    def close(self):
        self.closed = True


class DownloadModelArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="paddle-archive-test-")
        self.root = Path(self.temp.name)
        self.archive = self.root / "model.tar"
        self.destination = self.root / "model"
        self.destination.mkdir()

    def tearDown(self):
        self.temp.cleanup()

    def write_archive(self, members):
        with tarfile.open(self.archive, "w:") as bundle:
            for info, data in members:
                if data is None:
                    bundle.addfile(info)
                else:
                    bundle.addfile(info, io.BytesIO(data))

    def extract_members(self, members, model="sample"):
        self.write_archive(members)
        download_models.extract(self.archive, self.destination, model)

    def fake_fetch(self, url, payload=b"archive fixture", status=200, expected=None):
        response = FakeResponse(payload, status)
        connection = FakeHTTPSConnection(response)
        with mock.patch.object(download_models.http.client, "HTTPSConnection", return_value=connection) as create:
            download_models.fetch(
                url,
                self.root / "download.tar",
                expected if expected is not None else hashlib.sha256(payload).hexdigest(),
            )
        return create, connection

    def test_cached_archive_accepts_exact_streamed_sha256(self):
        payload = b"cached archive fixture"
        self.archive.write_bytes(payload)
        checksum = hashlib.sha256(payload).hexdigest()
        self.assertIsNone(download_models.verify_cached(self.archive, checksum))

    def test_cached_archive_rejects_corruption_size_and_symlink(self):
        payload = b"cached archive fixture"
        checksum = hashlib.sha256(payload).hexdigest()
        self.archive.write_bytes(b"corrupted archive fixture")
        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            download_models.verify_cached(self.archive, checksum)

        self.archive.write_bytes(payload)
        with mock.patch.object(download_models, "MAX_ARCHIVE_BYTES", len(payload) - 1):
            with self.assertRaisesRegex(ValueError, "cached model archive is invalid"):
                download_models.verify_cached(self.archive, checksum)

        link = self.root / "cached-link.tar"
        link.symlink_to(self.archive)
        with self.assertRaisesRegex(ValueError, "cached model archive is invalid"):
            download_models.verify_cached(link, checksum)

    def test_fetch_uses_fixed_trusted_https_host_and_checks_checksum(self):
        payload = b"checksum fixture"
        create, connection = self.fake_fetch(download_models.BASE_URL + "sample_infer.tar", payload)
        create.assert_called_once_with("paddle-model-ecology.bj.bcebos.com", timeout=90)
        self.assertEqual(connection.requests, [(
            "GET",
            "/paddlex/official_inference_model/paddle3.0.0/sample_infer.tar",
            {"User-Agent": "homelab-subtitle-converter/0.3"},
        )])
        self.assertTrue(connection.closed)
        self.assertEqual((self.root / "download.tar").read_bytes(), payload)
        (self.root / "download.tar").unlink()

        with self.assertRaisesRegex(ValueError, "checksum mismatch"):
            self.fake_fetch(download_models.BASE_URL + "sample_infer.tar", payload, expected="0" * 64)

    def test_fetch_rejects_untrusted_urls_without_network_access(self):
        invalid = [
            "http://paddle-model-ecology.bj.bcebos.com/paddlex/official_inference_model/paddle3.0.0/sample_infer.tar",
            "https://attacker.example/paddlex/official_inference_model/paddle3.0.0/sample_infer.tar",
            download_models.BASE_URL + "sample_infer.tar?redirect=1",
            download_models.BASE_URL + "sample_infer.tar#fragment",
        ]
        for url in invalid:
            with self.subTest(url=url), mock.patch.object(download_models.http.client, "HTTPSConnection") as create:
                with self.assertRaisesRegex(ValueError, "outside the trusted HTTPS source"):
                    download_models.fetch(url, self.root / "download.tar", "0" * 64)
                create.assert_not_called()

    def test_fetch_rejects_redirects_http_failures_and_oversized_archives(self):
        url = download_models.BASE_URL + "sample_infer.tar"
        for status in (301, 302, 404, 500):
            with self.subTest(status=status):
                response = FakeResponse(b"", status)
                connection = FakeHTTPSConnection(response)
                with mock.patch.object(download_models.http.client, "HTTPSConnection", return_value=connection):
                    with self.assertRaisesRegex(ValueError, "rejected the download or attempted a redirect"):
                        download_models.fetch(url, self.root / f"status-{status}.tar", "0" * 64)
                self.assertTrue(connection.closed)
                self.assertFalse((self.root / f"status-{status}.tar").exists())

        with mock.patch.object(download_models, "MAX_ARCHIVE_BYTES", 3):
            response = FakeResponse(b"oversized")
            connection = FakeHTTPSConnection(response)
            with mock.patch.object(download_models.http.client, "HTTPSConnection", return_value=connection):
                with self.assertRaisesRegex(ValueError, "archive exceeds size limit"):
                    download_models.fetch(url, self.root / "oversized.tar", "0" * 64)
            self.assertTrue(connection.closed)

    def test_primary_model_directory_layout_extracts(self):
        self.extract_members([
            regular("sample_infer/inference.json", b'{"model":true}'),
            regular("sample_infer/inference.pdiparams", b"small fixture weights"),
            regular("sample_infer/nested/labels.txt", b"label\n"),
        ])
        self.assertEqual((self.destination / "inference.json").read_bytes(), b'{"model":true}')
        self.assertEqual((self.destination / "inference.pdiparams").read_bytes(), b"small fixture weights")
        self.assertEqual((self.destination / "nested" / "labels.txt").read_text(encoding="ascii"), "label\n")

    def test_rejects_unsafe_paths_links_devices_duplicates_and_bounds(self):
        unsafe = []
        unsafe.append(("parent traversal", [regular("sample_infer/../escape", b"x"), regular("sample_infer/inference.json", b"{}")]))
        unsafe.append(("absolute path", [regular("/sample_infer/inference.json", b"{}")]))
        link = tarfile.TarInfo("sample_infer/link")
        link.type = tarfile.SYMTYPE
        link.linkname = "/outside"
        unsafe.append(("symlink", [(link, None), regular("sample_infer/inference.json", b"{}")]))
        device = tarfile.TarInfo("sample_infer/device")
        device.type = tarfile.CHRTYPE
        device.devmajor = 1
        device.devminor = 3
        unsafe.append(("device", [(device, None), regular("sample_infer/inference.json", b"{}")]))
        duplicate = regular("sample_infer/inference.json", b"{}")
        unsafe.append(("duplicate", [duplicate, regular("sample_infer/inference.json", b"{}")]))
        for label, members in unsafe:
            with self.subTest(label=label), tempfile.TemporaryDirectory(prefix="paddle-archive-case-") as case:
                self.archive = Path(case) / "archive.tar"
                self.destination = Path(case) / "destination"
                self.destination.mkdir()
                with self.assertRaises(ValueError):
                    self.extract_members(members)

        with tempfile.TemporaryDirectory(prefix="paddle-archive-size-") as case:
            self.archive = Path(case) / "archive.tar"
            self.destination = Path(case) / "destination"
            self.destination.mkdir()
            with mock.patch.object(download_models, "MAX_MODEL_BYTES", 4):
                with self.assertRaisesRegex(ValueError, "unpacked model exceeds size limit"):
                    self.extract_members([
                        regular("sample_infer/inference.json", b"12345"),
                    ])

        with tempfile.TemporaryDirectory(prefix="paddle-archive-count-") as case:
            self.archive = Path(case) / "archive.tar"
            self.destination = Path(case) / "destination"
            self.destination.mkdir()
            members = [regular(f"sample_infer/file{index}.bin", b"x") for index in range(3)]
            with mock.patch.object(download_models, "MAX_MEMBERS", 2):
                with self.assertRaisesRegex(ValueError, "invalid number of entries"):
                    self.extract_members(members)


if __name__ == "__main__":
    unittest.main()
