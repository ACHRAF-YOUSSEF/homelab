"""Disposable real-tool checks; opt in with CONVERTER_INTEGRATION=1."""

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

from converter import Converter, valid_srt


EN_SRT = "1\n00:00:00,500 --> 00:00:02,500\nHello from English\n\n2\n00:00:02,700 --> 00:00:04,700\nSecond English line\n"
FR_SRT = "1\n00:00:00,500 --> 00:00:02,500\nBonjour en français\n\n2\n00:00:02,700 --> 00:00:04,700\nDeuxième ligne\n"
AR_SRT = "1\n00:00:00,500 --> 00:00:02,500\nمرحبا\n\n2\n00:00:02,700 --> 00:00:04,700\nالسطر الثاني\n"


def tool(command):
    """Run a fixture-generation command and keep diagnostics fixture-only."""
    return subprocess.run(command, check=True, capture_output=True, timeout=180)


def make_text_variant(source, destination, fmt):
    tool(["ffmpeg", "-v", "error", "-y", "-i", str(source), str(destination)])
    if fmt == "ass":
        # FFmpeg has emitted ASS in its standard UTF-8 text representation.
        assert destination.read_text(encoding="utf-8").startswith("[Script Info]")
    else:
        assert destination.read_text(encoding="utf-8-sig").startswith("WEBVTT")


def make_image_subtitle(source, output_folder, fmt):
    output_folder.mkdir(parents=True)
    options = ["--overwrite", "--json", f"--output-folder:{output_folder}"]
    if fmt == "bluraysup":
        command = ["seconv", str(source), "bluraysup", "--resolution:1920x1080",
                   "--font-name:DejaVu Sans", "--font-size:64", *options]
    else:
        command = ["seconv", str(source), "vobsub", "--resolution:720x480",
                   "--font-name:DejaVu Sans", "--font-size:40", *options]
    result = tool(command)
    try:
        report = json.loads(result.stdout)
    except (UnicodeDecodeError, ValueError):
        report = {}
    if report.get("success") is False:
        raise AssertionError(f"seconv fixture generation failed: {report!r}")
    return list(output_folder.glob("*.sup")), list(output_folder.glob("*.idx"))


def vobsub_section(index_path, language, index_number, offset=0):
    text = index_path.read_text(encoding="utf-8-sig")
    matches = list(re.finditer(r"(?mi)^id:\s*([a-z-]+),\s*index:\s*\d+.*$", text))
    if not matches:
        raise AssertionError("generated IDX has no language section")
    match = matches[0]
    end = matches[1].start() if len(matches) > 1 else len(text)
    body = text[match.start():end]
    body = re.sub(r"(?mi)^id:.*$", f"id: {language}, index: {index_number}", body, count=1)
    if offset:
        body = re.sub(
            r"(?mi)(filepos:\s*)([0-9a-f]+)",
            lambda item: item[1] + f"{int(item[2], 16) + offset:09x}",
            body,
        )
    return text[:match.start()], body


@unittest.skipUnless(os.environ.get("CONVERTER_INTEGRATION") == "1", "set CONVERTER_INTEGRATION=1 in the pinned tool image")
class ConverterIntegrationTest(unittest.TestCase):
    def test_all_tracks_and_supported_sidecars_convert_without_source_changes(self):
        for executable in ("ffmpeg", "ffprobe", "mkvmerge", "seconv", "tesseract"):
            if not shutil.which(executable):
                self.skipTest(f"required fixture tool unavailable: {executable}")

        with tempfile.TemporaryDirectory(prefix="converter-integration-") as temp:
            base = Path(temp)
            library = base / "library"
            library.mkdir()
            state = base / "state.json"
            english = library / "source.en.srt"
            french = library / "source.fr.srt"
            arabic = library / "source.ar.srt"
            for path, content in ((english, EN_SRT), (french, FR_SRT), (arabic, AR_SRT)):
                path.write_text(content, encoding="utf-8")

            ass = library / "sidecar.en.ass"
            vtt = library / "sidecar.fr.vtt"
            make_text_variant(english, ass, "ass")
            make_text_variant(french, vtt, "vtt")

            movie = library / "Multi-language.mkv"
            tool([
                "ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "color=c=black:s=320x240:d=5:r=24",
                "-i", str(english), "-i", str(french), "-i", str(arabic),
                "-map", "0:v:0", "-map", "1:0", "-map", "2:0", "-map", "3:0",
                "-c:v", "mpeg4", "-c:s", "srt", "-metadata:s:s:0", "language=eng",
                "-metadata:s:s:1", "language=fra", "-metadata:s:s:2", "language=ara",
                "-disposition:s:1", "forced+hearing_impaired", str(movie),
            ])

            image_dir = base / "generated"
            sup_files, _ = make_image_subtitle(english, image_dir / "pgs", "bluraysup")
            vobsub_dir = image_dir / "vobsub"
            _, generated_idx = make_image_subtitle(english, vobsub_dir, "vobsub")
            if not sup_files or not generated_idx:
                self.fail("seconv did not generate the expected SUP and IDX fixtures")
            sup = library / "ocr.en.sup"
            shutil.copyfile(sup_files[0], sup)

            embedded_pgs = library / "Embedded-pgs.mkv"
            base_video = image_dir / "base-video.mkv"
            tool([
                "ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "color=c=black:s=320x240:d=5:r=24",
                "-c:v", "mpeg4", str(base_video),
            ])
            tool(["mkvmerge", "-o", str(embedded_pgs), str(base_video), "--language", "0:eng", str(sup)])

            _, generated_fr_idx = make_image_subtitle(french, image_dir / "vobsub-fr", "vobsub")
            if not generated_fr_idx:
                self.fail("seconv did not generate the French VobSub fixture")
            en_payload = generated_idx[0].with_suffix(".sub").read_bytes()
            fr_payload = generated_fr_idx[0].with_suffix(".sub").read_bytes()
            en_header, en_body = vobsub_section(generated_idx[0], "en", 0)
            _, fr_body = vobsub_section(generated_fr_idx[0], "fr", 1, len(en_payload))
            idx = library / "vobsub.idx"
            header = re.sub(r"(?mi)^langidx:.*$", "langidx: 0", en_header)
            idx.write_text(header + en_body.rstrip() + "\n" + fr_body, encoding="utf-8")
            idx.with_suffix(".sub").write_bytes(en_payload + fr_payload)

            source_files = [english, french, arabic, movie, embedded_pgs, ass, vtt, sup, idx, idx.with_suffix(".sub")]
            before_hashes = {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in source_files}
            converter = Converter([library], state, min_age=0, timeout=300, retry_interval=30)
            job_stage = base / "vobsub-jobs"
            job_stage.mkdir()
            vobsub_jobs = converter.jobs(idx, job_stage)
            self.assertEqual([job[1] for job in vobsub_jobs], ["vobsub.en.srt", "vobsub.fr.srt"], idx.read_text(encoding="utf-8"))

            first_scan = converter.scan()
            self.assertEqual(first_scan["failed"], 0, first_scan)
            expected = [
                "Multi-language.en.srt", "Multi-language.fr.forced.srt", "Multi-language.ar.srt",
                "Embedded-pgs.en.srt", "sidecar.en.srt", "sidecar.fr.srt", "ocr.en.srt",
                "vobsub.en.srt", "vobsub.fr.srt",
            ]
            for name in expected:
                output = library / name
                self.assertTrue(output.is_file(), f"missing {name}; tracks={converter.tracks(movie)!r}; outputs={[path.name for path in library.glob('*.srt')]!r}")
                self.assertTrue(valid_srt(output), f"invalid SRT {name}")
            self.assertIn("Hello", (library / "vobsub.en.srt").read_text(encoding="utf-8-sig"))
            self.assertIn("Bonjour", (library / "vobsub.fr.srt").read_text(encoding="utf-8-sig"))
            self.assertEqual(
                {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in source_files},
                before_hashes,
            )

            second_scan = converter.scan()
            self.assertEqual(second_scan["cached"], first_scan["converted"], second_scan)
            self.assertEqual(second_scan["converted"], 0, second_scan)


if __name__ == "__main__":
    unittest.main()
