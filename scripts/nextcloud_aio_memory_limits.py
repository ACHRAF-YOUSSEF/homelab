#!/usr/bin/env python3
"""Dry-run-first, fixed-allowlist memory limits for Nextcloud AIO children."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from typing import Any


TARGETS_MIB = {
    "nextcloud-aio-apache": 768,
    "nextcloud-aio-database": 1024,
    "nextcloud-aio-eurooffice": 3072,
    "nextcloud-aio-imaginary": 256,
    "nextcloud-aio-nextcloud": 1536,
    "nextcloud-aio-notify-push": 128,
    "nextcloud-aio-redis": 128,
    "nextcloud-aio-talk": 384,
    "nextcloud-aio-whiteboard": 384,
}
MIB = 1024 * 1024
STATE_PATH = Path(tempfile.gettempdir()) / "homelab-nextcloud-aio-memory-limits.json"
INSPECT_FORMAT = "{{.Id}}|{{.State.Running}}|{{.HostConfig.Memory}}|{{.HostConfig.MemorySwap}}"


class ToolError(RuntimeError):
    pass


def _number(value: Any, field: str, *, allow_unlimited: bool = False) -> int:
    if isinstance(value, bool):
        raise ToolError(f"Invalid Docker {field} value.")
    try:
        result = int(value)
    except (TypeError, ValueError):
        raise ToolError(f"Invalid Docker {field} value.") from None
    if result < (-1 if allow_unlimited else 0):
        raise ToolError(f"Invalid Docker {field} value.")
    return result


def parse_inspect_line(name: str, line: str) -> dict[str, Any]:
    fields = line.strip().split("|")
    if len(fields) != 4 or not fields[0]:
        raise ToolError(f"Docker returned an invalid resource record for {name}.")
    if fields[1] not in ("true", "false"):
        raise ToolError(f"Docker returned an invalid running state for {name}.")
    return {
        "name": name,
        "id": fields[0],
        "running": fields[1] == "true",
        "memory": _number(fields[2], "memory"),
        "memory_swap": _number(fields[3], "memory-swap", allow_unlimited=True),
    }


def build_plan(observed: dict[str, dict[str, Any]]) -> list[dict[str, Any]]:
    """Build a report only for the fixed service allowlist."""
    plan = []
    for name, target_mib in TARGETS_MIB.items():
        state = observed.get(name)
        if state is None:
            plan.append({"name": name, "present": False, "target_mib": target_mib})
            continue
        memory = _number(state.get("memory"), "memory")
        memory_swap = _number(state.get("memory_swap"), "memory-swap", allow_unlimited=True)
        plan.append({
            "name": name,
            "present": True,
            "id": str(state.get("id", "")),
            "running": bool(state.get("running", False)),
            "memory_bytes": memory,
            "memory_swap_bytes": memory_swap,
            "current_memory_mib": None if memory == 0 else round(memory / MIB, 2),
            "target_memory_mib": target_mib,
        })
    return plan


def inspect_one(name: str) -> dict[str, Any] | None:
    try:
        result = subprocess.run(
            ["docker", "inspect", "--format", INSPECT_FORMAT, name],
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError:
        raise ToolError("Could not run the Docker CLI.") from None
    if result.returncode != 0:
        return None
    return parse_inspect_line(name, result.stdout)


def inspect_allowlist() -> dict[str, dict[str, Any]]:
    try:
        probe = subprocess.run(
            ["docker", "info", "--format", "{{.ServerVersion}}"],
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError:
        raise ToolError("Could not run the Docker CLI.") from None
    if probe.returncode != 0 or not probe.stdout.strip():
        raise ToolError("Could not connect to the Docker daemon.")
    found = {}
    for name in TARGETS_MIB:
        state = inspect_one(name)
        if state is not None:
            found[name] = state
    return found


def docker_update(name: str, memory: int, memory_swap: int | None = None) -> None:
    command = ["docker", "update", f"--memory={memory}"]
    if memory_swap is not None:
        # 0 means unset/default; -1 means unlimited. Both are valid Docker values.
        command.append(f"--memory-swap={memory_swap}")
    command.append(name)
    try:
        result = subprocess.run(command, check=False, capture_output=True, text=True)
    except OSError:
        raise ToolError("Could not run the Docker CLI.") from None
    if result.returncode != 0:
        raise ToolError(f"Docker could not update the resource limits for {name}.")


def swap_limit_for_target(memory: int, memory_swap: int, target_memory: int) -> int:
    """Keep swap policy when changing memory; Docker swap is a combined cap."""
    if target_memory <= 0:
        raise ToolError("The target memory limit must be positive.")
    if memory_swap == -1:
        return -1
    if memory_swap == 0:
        # Docker's documented default is memory + an equal amount of swap.
        return target_memory * 2
    if memory > 0:
        # Preserve the prior swap allowance, not the old combined total.
        return target_memory + max(0, memory_swap - memory)
    # Defensive handling for a daemon record with swap set but no memory cap.
    return max(target_memory, memory_swap)


def _write_snapshot(snapshot: list[dict[str, Any]], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    payload = {"schema": 1, "containers": snapshot}
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(payload, stream, indent=2)
            stream.write("\n")
        os.replace(temporary, path)
    except Exception:
        try:
            temporary.unlink()
        except OSError:
            pass
        raise


def _read_snapshot(path: Path) -> list[dict[str, Any]]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        raise ToolError("Could not read the saved memory-limit restore point.") from None
    if not isinstance(payload, dict) or payload.get("schema") != 1 or not isinstance(payload.get("containers"), list):
        raise ToolError("The saved memory-limit restore point has an unsupported format.")
    entries = payload["containers"]
    seen = set()
    for entry in entries:
        if not isinstance(entry, dict):
            raise ToolError("The saved memory-limit restore point is invalid.")
        name = entry.get("name")
        if name not in TARGETS_MIB or name in seen or not isinstance(entry.get("id"), str) or not entry["id"]:
            raise ToolError("The saved restore point contains an unknown or duplicate container.")
        seen.add(name)
        _number(entry.get("memory"), "memory")
        _number(entry.get("memory_swap"), "memory-swap", allow_unlimited=True)
    if not entries:
        raise ToolError("The saved restore point contains no containers.")
    return entries


def _verify_state(name: str, expected_id: str, memory: int, memory_swap: int) -> None:
    current = inspect_one(name)
    if current is None or current["id"] != expected_id:
        raise ToolError(f"Container {name} was removed or recreated; refusing to apply a stale restore point.")
    if current["memory"] != memory or current["memory_swap"] != memory_swap:
        raise ToolError(f"Docker did not apply the requested memory and swap limits for {name}.")


def apply_limits(path: Path) -> dict[str, Any]:
    if path.exists():
        raise ToolError(f"A restore point already exists at {path}; restore it before applying again.")
    observed = inspect_allowlist()
    snapshot = list(observed.values())
    if not snapshot:
        raise ToolError("No containers from the fixed Nextcloud AIO allowlist were found.")
    _write_snapshot(snapshot, path)
    applied = []
    try:
        for state in snapshot:
            name = state["name"]
            target_memory = TARGETS_MIB[name] * MIB
            target_swap = swap_limit_for_target(state["memory"], state["memory_swap"], target_memory)
            docker_update(state["id"], target_memory, target_swap)
            current = inspect_one(name)
            if current is None or current["id"] != state["id"] or current["memory"] != target_memory:
                raise ToolError(f"Docker did not apply the requested memory limit for {name}.")
            if current["memory_swap"] != target_swap:
                raise ToolError(f"Docker did not apply the requested swap limit for {name}.")
            applied.append(name)
    except ToolError:
        rollback_errors = []
        for state in reversed(snapshot):
            try:
                current = inspect_one(state["name"])
                if current is None or current["id"] != state["id"]:
                    raise ToolError("Container was recreated; refusing a stale rollback")
                docker_update(state["id"], state["memory"], state["memory_swap"])
            except ToolError:
                rollback_errors.append(state["name"])
        if rollback_errors:
            raise ToolError("Apply failed and automatic rollback was incomplete; restore point retained for: " + ", ".join(rollback_errors)) from None
        path.unlink(missing_ok=True)
        raise
    return {"action": "applied", "updated": applied, "restore_point": str(path)}


def restore_limits(path: Path) -> dict[str, Any]:
    snapshot = _read_snapshot(path)
    current = inspect_allowlist()
    # Validate every container before changing any of them.
    for state in snapshot:
        present = current.get(state["name"])
        if present is None or present["id"] != state["id"]:
            raise ToolError(f"Container {state['name']} was removed or recreated; refusing to restore stale limits.")
    restored = []
    for state in snapshot:
        docker_update(state["id"], state["memory"], state["memory_swap"])
        _verify_state(state["name"], state["id"], state["memory"], state["memory_swap"])
        restored.append(state["name"])
    path.unlink()
    return {"action": "restored", "updated": restored}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_mutually_exclusive_group()
    actions.add_argument("--apply", action="store_true", help="apply fixed memory caps and save a restore point")
    actions.add_argument("--restore", action="store_true", help="restore the saved memory and swap settings")
    parser.add_argument("--state-file", type=Path, default=STATE_PATH, help="restore-point file path")
    parser.add_argument("--fixture", type=Path, help="read inspect JSON fixture for an offline dry run only")
    args = parser.parse_args(argv)

    try:
        if args.fixture is not None:
            if args.apply or args.restore:
                raise ToolError("Fixtures are supported for dry runs only.")
            try:
                fixture = json.loads(args.fixture.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                raise ToolError("Could not read the Docker inspect fixture.") from None
            if not isinstance(fixture, dict) or set(fixture) - set(TARGETS_MIB):
                raise ToolError("The fixture must be an object keyed only by the fixed container allowlist.")
            result = {"action": "dry-run", "containers": build_plan(fixture)}
        elif args.apply:
            result = apply_limits(args.state_file)
        elif args.restore:
            result = restore_limits(args.state_file)
        else:
            result = {"action": "dry-run", "containers": build_plan(inspect_allowlist())}
        print(json.dumps(result, indent=2))
        return 0
    except ToolError as exc:
        print(json.dumps({"error": str(exc)}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
