"""Offline adapter tests using fake Paddle modules and disposable images."""
from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import struct
import sys
import tempfile
import types
import unittest
from unittest import mock
import zlib


GPU_DIR = Path(__file__).resolve().parent
ADAPTER_PATH = GPU_DIR / "paddleocr"
_loader = importlib.machinery.SourceFileLoader("subtitle_paddleocr_adapter", str(ADAPTER_PATH))
_spec = importlib.util.spec_from_loader(_loader.name, _loader)
adapter = importlib.util.module_from_spec(_spec)
sys.modules[_loader.name] = adapter
_loader.exec_module(adapter)


def tiny_png() -> bytes:
    """Return a valid, single-pixel RGB PNG without image-library dependencies."""
    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">2I5B", 1, 1, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(b"\x00\x20\x40\x60"))
        + chunk(b"IEND", b"")
    )


class FakeCuda:
    def __init__(self, *, count: int = 1):
        self.count = count
        self.synced = []

    def device_count(self):
        return self.count

    def synchronize(self, index):
        self.synced.append(index)

def fake_paddle(*, compiled: bool = True, count: int = 1, place: str = "gpu:0"):
    cuda = FakeCuda(count=count)
    paddle = types.ModuleType("paddle")
    paddle.CUDAPlace = lambda index: place
    paddle.device = types.SimpleNamespace(
        is_compiled_with_cuda=lambda: compiled,
        cuda=cuda,
        set_device=lambda device: setattr(paddle, "selected_device", device),
    )
    paddle.to_tensor = lambda values, place=None: types.SimpleNamespace(place=place)
    return paddle, cuda


class MockExecutablePath(str):
    def is_file(self):
        return self == "/usr/bin/nvidia-smi"


class AdapterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="paddle-adapter-test-")
        self.root = Path(self.temp.name)
        self.input_dir = self.root / "input"
        self.output_dir = self.root / "output"
        self.input_dir.mkdir()
        self.output_dir.mkdir()
        self.models = self.root / "models"
        self.models.mkdir()
        self.model_root_patch = mock.patch.object(adapter, "MODEL_ROOT", self.models)
        self.model_root_patch.start()
        self.addCleanup(self.model_root_patch.stop)
        self.env_patch = mock.patch.dict(os.environ, {
            "GPU_MIN_FREE_MIB": "1",
            "TMPDIR": str(self.root),
        })
        self.env_patch.start()
        self.addCleanup(self.env_patch.stop)

    def tearDown(self):
        self.temp.cleanup()

    def install_models(self, language: str) -> dict:
        names = {
            "PP-OCRv6_small_det", "PP-OCRv6_small_rec", "PP-OCRv5_mobile_det",
            "korean_PP-OCRv5_mobile_rec", "pt_PP-OCRv5_mobile_rec",
        }
        for name in names:
            target = self.models / name
            target.mkdir(parents=True, exist_ok=True)
            (target / "inference.json").write_text("{}", encoding="utf-8")
        return adapter._models(language)

    @contextlib.contextmanager
    def modules(self, paddle, paddleocr=None):
        cv2 = types.ModuleType("cv2")
        cv2.setNumThreads = mock.Mock()
        modules = {"paddle": paddle, "cv2": cv2}
        if paddleocr is not None:
            modules["paddleocr"] = paddleocr
        with mock.patch.dict(sys.modules, modules), mock.patch.object(
            adapter, "_gpu_memory", return_value=(8 * 1024**3, 12 * 1024**3)
        ):
            yield

    def test_cuda_probe_fails_closed_for_cpu_build_missing_gpu_and_cpu_tensor(self):
        cases = [
            (fake_paddle(compiled=False)[0], "without CUDA support"),
            (fake_paddle(count=0)[0], "exactly one CUDA GPU must be available"),
            (fake_paddle(count=2)[0], "exactly one CUDA GPU must be available"),
            (fake_paddle(place="Place(cpu)")[0], "not allocated on GPU 0"),
        ]
        for paddle, message in cases:
            with self.subTest(message=message), self.modules(paddle):
                with self.assertRaisesRegex(adapter.AdapterError, message):
                    adapter._cuda_probe(paddle)

    def test_cuda_probe_enforces_free_memory_threshold(self):
        paddle, _ = fake_paddle()
        with self.modules(paddle), mock.patch.object(adapter, "_gpu_memory", return_value=(2 * 1024**2, 8 * 1024**2)), mock.patch.dict(os.environ, {"GPU_MIN_FREE_MIB": "3"}):
            with self.assertRaisesRegex(adapter.AdapterError, "configured free-memory threshold"):
                adapter._cuda_probe(paddle)
        with self.modules(paddle), mock.patch.dict(os.environ, {"GPU_MIN_FREE_MIB": "bad"}):
            with self.assertRaisesRegex(adapter.AdapterError, "invalid configured GPU free-memory threshold"):
                adapter._cuda_probe(paddle)

    def test_gpu_memory_uses_fixed_nvidia_smi_arguments(self):
        completed = types.SimpleNamespace(stdout="8192, 12288\n")
        with mock.patch.object(adapter, "Path", MockExecutablePath), mock.patch.object(adapter.subprocess, "run", return_value=completed) as run:
            self.assertEqual(adapter._gpu_memory(), (8192 * 1024**2, 12288 * 1024**2))
        run.assert_called_once_with(
            ["/usr/bin/nvidia-smi", "--id=0", "--query-gpu=memory.free,memory.total", "--format=csv,noheader,nounits"],
            capture_output=True,
            text=True,
            check=True,
            timeout=5,
        )

    def test_gpu_memory_rejects_malformed_output(self):
        malformed = ["bad,100\n", "1,2\n3,4\n", "1,0\n", "3,2\n", "1,2,3\n", "1" * 129]
        for output in malformed:
            with self.subTest(output=output[:12]), mock.patch.object(adapter, "Path", MockExecutablePath), mock.patch.object(adapter.subprocess, "run", return_value=types.SimpleNamespace(stdout=output)):
                with self.assertRaisesRegex(adapter.AdapterError, "invalid NVIDIA memory query response"):
                    adapter._gpu_memory()

    def test_health_is_single_clean_json_and_does_not_check_models(self):
        paddle, cuda = fake_paddle()
        output = io.StringIO()
        with self.modules(paddle), contextlib.redirect_stdout(output):
            self.assertEqual(adapter.health(), 0)
        lines = output.getvalue().splitlines()
        self.assertEqual(len(lines), 1)
        value = json.loads(lines[0])
        self.assertEqual(value["ok"], True)
        self.assertEqual(value["models_checked"], False)
        self.assertEqual(value["device_count"], 1)
        self.assertEqual(cuda.synced, [0])

    def test_health_returns_sanitized_failure_json(self):
        paddle, _ = fake_paddle(compiled=False)
        output = io.StringIO()
        with self.modules(paddle), contextlib.redirect_stdout(output):
            self.assertEqual(adapter.health(), 1)
        self.assertEqual(json.loads(output.getvalue()), {
            "ok": False,
            "error": "PaddlePaddle was built without CUDA support",
        })

    def test_language_aliases_select_expected_model_families(self):
        expected = {
            "ja": ("PP-OCRv6_small_det", "PP-OCRv6_small_rec"),
            "ko": ("PP-OCRv5_mobile_det", "korean_PP-OCRv5_mobile_rec"),
            "zh-TW": ("PP-OCRv6_small_det", "PP-OCRv6_small_rec"),
            "pt-BR": ("PP-OCRv6_small_det", "PP-OCRv6_small_rec"),
        }
        for language, names in expected.items():
            with self.subTest(language=language):
                result = self.install_models(language)
                self.assertEqual((result["text_detection_model_name"], result["text_recognition_model_name"]), names)
        with self.assertRaisesRegex(adapter.AdapterError, "unsupported OCR language: xx-unsupported"):
            adapter._models("xx-unsupported")

    def test_missing_model_files_fail_before_paddleocr_construction(self):
        paddle, _ = fake_paddle()
        fake_ocr = types.ModuleType("paddleocr")
        fake_ocr.PaddleOCR = mock.Mock(side_effect=AssertionError("model validation must run first"))
        (self.input_dir / "one.png").write_bytes(tiny_png())
        with self.modules(paddle, fake_ocr):
            with self.assertRaisesRegex(adapter.AdapterError, "required packaged OCR model is unavailable"):
                adapter.run(self.input_dir, self.output_dir, "en")
        fake_ocr.PaddleOCR.assert_not_called()

    def test_cli_accepts_legacy_false_and_rejects_true_or_cpu_device(self):
        self.install_models("en")
        paddle, _ = fake_paddle()
        fake_ocr = self.fake_paddleocr()
        (self.input_dir / "one.png").write_bytes(tiny_png())
        prefix = ["paddleocr", "ocr", "-i", str(self.input_dir), "--lang", "en", "--save_path", str(self.output_dir)]
        with self.modules(paddle, fake_ocr), mock.patch.object(sys, "argv", prefix + ["--use_angle_cls", "false"]):
            self.assertEqual(adapter.main(), 0)
        with self.modules(paddle, fake_ocr), mock.patch.object(sys, "argv", prefix + ["--use_angle_cls", "true"]):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                adapter.main()
            self.assertEqual(error.exception.code, 2)
        with self.modules(paddle, fake_ocr), mock.patch.object(sys, "argv", prefix + ["--device", "cpu"]):
            output = io.StringIO()
            with contextlib.redirect_stderr(output):
                self.assertEqual(adapter.main(), 2)
            self.assertIn("unsupported arguments", output.getvalue())

    def fake_paddleocr(self):
        module = types.ModuleType("paddleocr")
        module.calls = []
        module.events = []

        class Result:
            def __init__(self, source):
                self.source = source

            def save_to_json(self, destination):
                module.events.append(("save", Path(destination)))
                output = Path(destination) / (Path(self.source).stem + ".json")
                output.write_text(json.dumps({"res": {"input_path": self.source, "rec_texts": ["fixture"]}}), encoding="utf-8")

        class PaddleOCR:
            def __init__(self, **kwargs):
                module.calls.append(kwargs)

            def predict_iter(self, sources):
                module.events.append(("predict_iter", sources))
                for source in sources:
                    module.events.append(("yield", source))
                    yield Result(source)

        module.PaddleOCR = PaddleOCR
        return module

    def test_run_skips_symlinked_images_streams_results_and_batches_recognition(self):
        self.install_models("en")
        paddle, _ = fake_paddle()
        fake_ocr = self.fake_paddleocr()
        (self.input_dir / "b.png").write_bytes(tiny_png())
        (self.input_dir / "a.PNG").write_bytes(tiny_png())
        outside = self.root / "outside.png"
        outside.write_bytes(tiny_png())
        (self.input_dir / "linked.png").symlink_to(outside)
        with self.modules(paddle, fake_ocr):
            self.assertEqual(adapter.run(self.input_dir, self.output_dir, "en"), 0)
            sys.modules["cv2"].setNumThreads.assert_called_once_with(1)
        calls = fake_ocr.calls
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["device"], "gpu:0")
        self.assertEqual(calls[0]["text_recognition_batch_size"], 8)
        self.assertEqual(calls[0]["cpu_threads"], 1)
        self.assertEqual(calls[0]["text_detection_model_name"], "PP-OCRv6_small_det")
        events = fake_ocr.events
        predictions = [event[1] for event in events if event[0] == "predict_iter"]
        self.assertEqual(len(predictions), 1)
        self.assertEqual([Path(source).name for source in predictions[0]], ["a.PNG", "b.png"])
        self.assertEqual([Path(event[1]).name for event in events if event[0] == "yield"], ["a.PNG", "b.png"])
        self.assertEqual([event[0] for event in events], ["predict_iter", "yield", "save", "yield", "save"])
        for name in ("a.json", "b.json"):
            saved = json.loads((self.output_dir / name).read_text(encoding="utf-8"))
            self.assertEqual(saved["res"]["rec_texts"], ["fixture"])

    def test_input_output_must_be_inside_tmpdir_without_symlink_ancestry(self):
        with tempfile.TemporaryDirectory(prefix="paddle-outside-test-", dir=self.root.parent) as outside:
            outside_root = Path(outside)
            outside_input = outside_root / "input"
            outside_output = outside_root / "output"
            outside_input.mkdir()
            outside_output.mkdir()
            outside_input_link = self.root / "outside-input-link"
            outside_input_link.symlink_to(outside_input, target_is_directory=True)
            outside_output_link = self.root / "outside-output-link"
            outside_output_link.symlink_to(outside_output, target_is_directory=True)

            with self.assertRaises(adapter.AdapterError):
                adapter.run(outside_input, self.output_dir, "en")
            with self.assertRaises(adapter.AdapterError):
                adapter.run(self.input_dir, outside_output, "en")
            with self.assertRaises(adapter.AdapterError):
                adapter.run(outside_input_link, self.output_dir, "en")
            with self.assertRaises(adapter.AdapterError):
                adapter.run(self.input_dir, outside_output_link, "en")

        with self.assertRaises(adapter.AdapterError):
            adapter.run(self.input_dir, self.input_dir / "nested-output", "en")

    def test_stream_rejects_missing_or_extra_results(self):
        self.install_models("en")
        paddle, _ = fake_paddle()
        (self.input_dir / "one.png").write_bytes(tiny_png())
        for count in (0, 2):
            fake_ocr = types.ModuleType("paddleocr")
            result = types.SimpleNamespace(save_to_json=mock.Mock())
            instance = types.SimpleNamespace(predict_iter=lambda sources: iter([result] * count))
            fake_ocr.PaddleOCR = mock.Mock(return_value=instance)
            with self.subTest(count=count), self.modules(paddle, fake_ocr):
                with self.assertRaisesRegex(adapter.AdapterError, "results|every input image"):
                    adapter.run(self.input_dir, self.output_dir, "en")
            self.assertLessEqual(result.save_to_json.call_count, 1)

    def test_symlinked_tmpdir_ancestry_is_rejected(self):
        target = self.root / "tmp-target"
        target.mkdir()
        (target / "input").mkdir()
        (target / "output").mkdir()
        tmp_link = self.root / "tmp-link"
        tmp_link.symlink_to(target, target_is_directory=True)
        with mock.patch.dict(os.environ, {"TMPDIR": str(tmp_link)}):
            with self.assertRaises(adapter.AdapterError):
                adapter.run(tmp_link / "input", tmp_link / "output", "en")

    def test_image_count_limit_is_enforced_before_model_imports(self):
        (self.input_dir / "a.png").write_bytes(tiny_png())
        (self.input_dir / "b.png").write_bytes(tiny_png())
        (self.input_dir / "c.png").write_bytes(tiny_png())
        with mock.patch.object(adapter, "MAX_IMAGES", 2):
            with self.assertRaisesRegex(adapter.AdapterError, "2-image limit"):
                adapter.run(self.input_dir, self.output_dir, "en")


if __name__ == "__main__":
    unittest.main()
