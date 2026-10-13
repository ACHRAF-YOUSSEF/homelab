import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


TOOL_PATH = Path(__file__).with_name("compose_memory_limits.py")
SPEC = importlib.util.spec_from_file_location("compose_memory_tool", TOOL_PATH)
TOOL = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(TOOL)


class ComposeMemoryToolTests(unittest.TestCase):
    def test_reads_literals_and_environment_defaults_only_from_services(self):
        with tempfile.TemporaryDirectory() as directory:
            compose = Path(directory)
            (compose / "one.yml").write_text(
                "services:\n  alpha:\n    mem_limit: 128m\n  beta:\n    mem_limit: ${BETA_LIMIT:-2g}\n",
                encoding="utf-8",
            )
            self.assertEqual(
                TOOL.compose_caps(compose, {"alpha", "beta"}),
                {"alpha": 128 * TOOL.MIB, "beta": 2 * 1024 * TOOL.MIB},
            )

    def test_updates_target_immutable_id_and_detects_recreated_container(self):
        record = {"id": "immutable-id", "service": "open-webui"}
        observed = {"id": "immutable-id", "service": "open-webui", "memory": 0, "memory_swap": 0}
        updated = {**observed, "memory": 128 * TOOL.MIB, "memory_swap": 256 * TOOL.MIB}
        commands = []
        with patch.object(TOOL, "inspect_id", side_effect=[observed, updated]), patch.object(
            TOOL, "docker_update", side_effect=lambda *args: commands.append(args)
        ):
            TOOL.verify_id_and_update(record, 128 * TOOL.MIB, 256 * TOOL.MIB)
        self.assertEqual(commands, [("immutable-id", 128 * TOOL.MIB, 256 * TOOL.MIB)])

        with patch.object(TOOL, "inspect_id", return_value={**observed, "id": "different-id"}), patch.object(
            TOOL, "docker_update"
        ) as update:
            with self.assertRaises(TOOL.ToolError):
                TOOL.verify_id_and_update(record, 128 * TOOL.MIB, 256 * TOOL.MIB)
            update.assert_not_called()

    def test_stale_container_during_rollback_retains_restore_point(self):
        first = {"id": "first-id", "service": "open-webui", "memory": 0, "memory_swap": 0}
        second = {"id": "second-id", "service": "searxng", "memory": 0, "memory_swap": 0}
        caps = {"open-webui": 128 * TOOL.MIB, "searxng": 128 * TOOL.MIB}
        with tempfile.TemporaryDirectory() as directory:
            state_file = Path(directory) / "state.json"
            calls = []

            def update(entry, memory, swap):
                calls.append((entry["id"], memory, swap))
                if entry["id"] == "second-id":
                    raise TOOL.ToolError("simulated update failure")
                if len(calls) > 2:
                    raise TOOL.ToolError("container was recreated")

            with patch.object(TOOL, "discover", return_value=[first, second]), patch.object(
                TOOL, "verify_id_and_update", side_effect=update
            ):
                with self.assertRaisesRegex(TOOL.ToolError, "rollback was incomplete"):
                    TOOL.apply(caps, state_file)
            self.assertTrue(state_file.exists())
            self.assertEqual(calls[-1][0], "first-id")
            self.assertEqual(TOOL.read_snapshot(state_file), [first, second])

    def test_swap_defaults_and_unlimited_round_trip(self):
        target = 192 * TOOL.MIB
        self.assertEqual(TOOL.swap_target(0, 0, target), 2 * target)
        self.assertEqual(TOOL.swap_target(target, -1, target), -1)
        self.assertEqual(TOOL.swap_target(256 * TOOL.MIB, 512 * TOOL.MIB, target), target + 256 * TOOL.MIB)

    def test_ocr_flag_inspection_requests_only_the_specific_toggle(self):
        with patch.object(TOOL, "run_docker", return_value="enabled") as run:
            self.assertTrue(TOOL.converter_ocr_enabled("container-id"))
        command = run.call_args.args[0]
        self.assertEqual(
            command[3],
            '{{range .Config.Env}}{{if eq . "CONVERTER_ENABLE_OCR=true"}}enabled{{end}}{{end}}',
        )


if __name__ == "__main__":
    unittest.main()
