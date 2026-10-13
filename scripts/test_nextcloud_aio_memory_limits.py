import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


TOOL_PATH = Path(__file__).with_name("nextcloud_aio_memory_limits.py")
SPEC = importlib.util.spec_from_file_location("aio_memory_tool", TOOL_PATH)
TOOL = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(TOOL)


class AioMemoryToolTests(unittest.TestCase):
    def test_inspect_parses_unlimited_memory_and_swap_values(self):
        state = TOOL.parse_inspect_line("nextcloud-aio-redis", "abc123|true|0|0")
        self.assertEqual(state["memory"], 0)
        self.assertEqual(state["memory_swap"], 0)
        self.assertTrue(state["running"])

        unlimited_swap = TOOL.parse_inspect_line("nextcloud-aio-redis", "abc123|false|0|-1")
        self.assertEqual(unlimited_swap["memory_swap"], -1)
        self.assertFalse(unlimited_swap["running"])

    def test_plan_is_limited_to_fixed_allowlist_and_reports_current_and_target(self):
        fixture = {
            "nextcloud-aio-eurooffice": {
                "id": "fixture-id",
                "running": True,
                "memory": 2_368_000_000,
                "memory_swap": 0,
            },
            "unrelated-container": {"id": "ignored"},
        }
        plan = TOOL.build_plan(fixture)
        eurooffice = next(item for item in plan if item["name"] == "nextcloud-aio-eurooffice")
        self.assertEqual(eurooffice["target_memory_mib"], 3072)
        self.assertEqual(eurooffice["memory_swap_bytes"], 0)
        self.assertTrue(eurooffice["present"])
        self.assertFalse(any(item["name"] == "unrelated-container" for item in plan))
        self.assertEqual(len(plan), len(TOOL.TARGETS_MIB))

    def test_fixture_cli_emits_json_dry_run_without_docker(self):
        fixture = {
            name: {"id": f"fixture-{index}", "running": True, "memory": 0, "memory_swap": 0}
            for index, name in enumerate(TOOL.TARGETS_MIB)
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "inspect.json"
            path.write_text(json.dumps(fixture), encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(TOOL_PATH), "--fixture", str(path)],
                check=True,
                capture_output=True,
                text=True,
            )
        parsed = json.loads(result.stdout)
        self.assertEqual(parsed["action"], "dry-run")
        self.assertEqual(len(parsed["containers"]), 9)
        eurooffice = next(row for row in parsed["containers"] if row["name"] == "nextcloud-aio-eurooffice")
        self.assertIsNone(eurooffice["current_memory_mib"])
        self.assertEqual(eurooffice["memory_swap_bytes"], 0)

    def test_memory_swap_rejects_values_below_minus_one(self):
        with self.assertRaises(TOOL.ToolError):
            TOOL.parse_inspect_line("nextcloud-aio-redis", "abc123|true|0|-2")

    def test_target_swap_preserves_unlimited_or_existing_swap_allowance(self):
        target = 768 * TOOL.MIB
        self.assertEqual(TOOL.swap_limit_for_target(0, -1, target), -1)
        self.assertEqual(TOOL.swap_limit_for_target(0, 0, target), target * 2)
        self.assertEqual(TOOL.swap_limit_for_target(512 * TOOL.MIB, 1024 * TOOL.MIB, target), target + 512 * TOOL.MIB)
        self.assertEqual(TOOL.swap_limit_for_target(512 * TOOL.MIB, 768 * TOOL.MIB, target), target + 256 * TOOL.MIB)
        self.assertEqual(TOOL.swap_limit_for_target(0, 2048 * TOOL.MIB, target), 2048 * TOOL.MIB)

    def test_restore_snapshot_round_trips_unlimited_and_finite_swap_fields(self):
        snapshot = [
            {"name": "nextcloud-aio-redis", "id": "redis-id", "memory": 0, "memory_swap": 0},
            {"name": "nextcloud-aio-eurooffice", "id": "office-id", "memory": 2_400_000_000, "memory_swap": -1},
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "restore.json"
            TOOL._write_snapshot(snapshot, path)
            restored = TOOL._read_snapshot(path)
        self.assertEqual(restored, snapshot)


if __name__ == "__main__":
    unittest.main()
