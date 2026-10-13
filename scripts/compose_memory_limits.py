#!/usr/bin/env python3
"""Dry-run-first memory caps for this repository's Compose project.

Only containers carrying the fixed Compose project label and a service name
with an explicit ``mem_limit`` in ``compose/*.yml`` are considered. Applying
limits is opt-in and saves exact memory/swap values for restoration.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from typing import Any


PROJECT = "homelab"
SERVICE_ALLOWLIST = {
    "open-webui", "searxng", "n8n", "subtitle-worker", "unbound", "adguardhome",
    "tunnel", "cloudflare-ddns", "docker-proxy", "portracker", "nginx-proxy-manager",
    "jellyfin-counts-cache", "glance", "glance-live", "uptime-kuma", "qbittorrent",
    "prowlarr", "sonarr", "radarr", "seerr", "bazarr", "subtitle-converter", "tdarr",
    "metube", "tubearchivist", "archivist-redis", "archivist-es",
    "nextcloud-docker-proxy", "nextcloud-aio-mastercontainer", "hbbs", "hbbr",
    "it-tools", "stirling-pdf",
}
MIB = 1024 * 1024
OCR_MINIMUM_MEMORY = 1536 * MIB
STATE_PATH = Path(tempfile.gettempdir()) / "homelab-compose-memory-limits.json"
ROOT = Path(__file__).resolve().parents[1]
SERVICE_LINE = re.compile(r"^  ([A-Za-z0-9_-]+):\s*$")
MEM_LINE = re.compile(r"^    mem_limit:\s*(\S+)\s*$")
ENV_DEFAULT = re.compile(r"^\$\{[A-Za-z_][A-Za-z0-9_]*:-([^}]+)}$")
SIZE = re.compile(r"^(\d+)([bkmg]?)$", re.IGNORECASE)
INSPECT_FORMAT = (
    '{{.Id}}|{{.State.Running}}|{{.HostConfig.Memory}}|{{.HostConfig.MemorySwap}}|'
    '{{index .Config.Labels "com.docker.compose.service"}}|'
    '{{index .Config.Labels "com.docker.compose.project"}}'
)


class ToolError(RuntimeError):
    pass


def parse_size(value: str) -> int:
    default = ENV_DEFAULT.fullmatch(value)
    if default:
        value = default.group(1)
    match = SIZE.fullmatch(value)
    if not match:
        raise ToolError("A Compose mem_limit value has unsupported syntax.")
    amount = int(match.group(1))
    multiplier = {"": 1, "b": 1, "k": 1024, "m": MIB, "g": 1024 * MIB}[match.group(2).lower()]
    return amount * multiplier


def compose_caps(compose_dir: Path | None = None, expected_services: set[str] | None = None) -> dict[str, int]:
    """Read only explicit service names and mem_limit lines from Compose YAML."""
    directory = compose_dir or ROOT / "compose"
    caps: dict[str, int] = {}
    for path in sorted(directory.glob("*.yml")):
        in_services = False
        current: str | None = None
        for line in path.read_text(encoding="utf-8").splitlines():
            if line == "services:":
                in_services, current = True, None
                continue
            if in_services and line and not line.startswith(" "):
                in_services, current = False, None
            if not in_services:
                continue
            service_match = SERVICE_LINE.match(line)
            if service_match:
                current = service_match.group(1)
                continue
            limit_match = MEM_LINE.match(line)
            if limit_match and current:
                if current in caps:
                    raise ToolError("A Compose service has more than one declared memory limit.")
                caps[current] = parse_size(limit_match.group(1))
    expected = SERVICE_ALLOWLIST if expected_services is None else expected_services
    if set(caps) != expected:
        missing = sorted(expected - set(caps))
        unexpected = sorted(set(caps) - expected)
        raise ToolError(f"Compose memory-limit allowlist mismatch (missing={missing}, unexpected={unexpected}).")
    return caps


def run_docker(command: list[str]) -> str:
    try:
        result = subprocess.run(command, check=False, capture_output=True, text=True)
    except OSError:
        raise ToolError("Could not run the Docker CLI.") from None
    if result.returncode != 0:
        raise ToolError("The Docker CLI request failed; check the daemon and project state.")
    return result.stdout


def inspect_id(container_id: str) -> dict[str, Any]:
    line = run_docker(["docker", "inspect", "--format", INSPECT_FORMAT, container_id]).strip()
    fields = line.split("|")
    if len(fields) != 6 or not fields[0] or fields[1] not in ("true", "false"):
        raise ToolError("Docker returned an invalid container resource record.")
    service, project = fields[4], fields[5]
    if project != PROJECT:
        raise ToolError("Docker returned a container outside the fixed Compose project.")
    return {
        "id": fields[0], "running": fields[1] == "true", "service": service,
        "memory": _integer(fields[2], "memory"),
        "memory_swap": _integer(fields[3], "memory-swap", allow_unlimited=True),
    }


def converter_ocr_enabled(container_id: str) -> bool:
    """Read only the OCR toggle from the converter's environment, never other envs."""
    output = run_docker([
        "docker", "inspect", "--format",
        '{{range .Config.Env}}{{if or (eq . "CONVERTER_ENABLE_OCR=true") (eq . "CONVERTER_ENABLE_OCR=1")}}enabled{{end}}{{end}}',
        container_id,
    ])
    return output.strip() == "enabled"


def _integer(value: str, field: str, *, allow_unlimited: bool = False) -> int:
    try:
        result = int(value)
    except ValueError:
        raise ToolError(f"Docker returned an invalid {field} value.") from None
    if result < (-1 if allow_unlimited else 0):
        raise ToolError(f"Docker returned an invalid {field} value.")
    return result


def discover(caps: dict[str, int]) -> list[dict[str, Any]]:
    ids = run_docker([
        "docker", "ps", "-aq", "--filter", f"label=com.docker.compose.project={PROJECT}"
    ])
    found: dict[str, dict[str, Any]] = {}
    for container_id in ids.splitlines():
        if not container_id.strip():
            continue
        state = inspect_id(container_id.strip())
        if state["service"] not in caps:
            continue
        if state["service"] in found:
            raise ToolError(f"More than one container is labeled as Compose service {state['service']}.")
        found[state["service"]] = state
    return sorted(found.values(), key=lambda item: item["service"])


def plan(caps: dict[str, int], found: list[dict[str, Any]]) -> dict[str, Any]:
    present = {item["service"]: item for item in found}
    return {"action": "dry-run", "project": PROJECT, "containers": [
        {
            "service": service,
            "present": service in present,
            "container_id": present[service]["id"][:12] if service in present else None,
            "running": present[service]["running"] if service in present else None,
            "current_memory_mib": round(present[service]["memory"] / MIB, 2) if service in present and present[service]["memory"] else None,
            "current_memory_swap_mib": round(present[service]["memory_swap"] / MIB, 2) if service in present and present[service]["memory_swap"] > 0 else present[service]["memory_swap"] if service in present else None,
            "target_memory_mib": caps[service] / MIB,
        }
        for service in sorted(caps)
    ]}


def swap_target(memory: int, memory_swap: int, target_memory: int) -> int:
    if memory_swap == -1:
        return -1
    if memory_swap == 0:
        return target_memory * 2
    return target_memory + max(0, memory_swap - memory) if memory else max(target_memory, memory_swap)


def docker_update(container_id: str, memory: int, memory_swap: int) -> None:
    run_docker(["docker", "update", f"--memory={memory}", f"--memory-swap={memory_swap}", container_id])


def write_snapshot(path: Path, entries: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temp_path = path.with_suffix(path.suffix + ".tmp")
    descriptor = os.open(temp_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
            json.dump({"schema": 1, "project": PROJECT, "containers": entries}, stream, indent=2)
            stream.write("\n")
        os.replace(temp_path, path)
    except Exception:
        temp_path.unlink(missing_ok=True)
        raise


def read_snapshot(path: Path) -> list[dict[str, Any]]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        raise ToolError("Could not read the saved Compose memory restore point.") from None
    if not isinstance(payload, dict) or payload.get("schema") != 1 or payload.get("project") != PROJECT or not isinstance(payload.get("containers"), list):
        raise ToolError("The saved Compose memory restore point has an unsupported format.")
    entries = payload["containers"]
    seen: set[str] = set()
    for item in entries:
        if not isinstance(item, dict) or item.get("service") not in SERVICE_ALLOWLIST:
            raise ToolError("The saved restore point contains an unknown service.")
        if item["service"] in seen or not isinstance(item.get("id"), str) or not item["id"]:
            raise ToolError("The saved restore point contains a duplicate or invalid container record.")
        seen.add(item["service"])
        _integer(item.get("memory"), "memory")
        _integer(item.get("memory_swap"), "memory-swap", allow_unlimited=True)
    if not entries:
        raise ToolError("No managed Compose containers were present; nothing to apply or restore.")
    return entries


def verify_id_and_update(entry: dict[str, Any], memory: int, memory_swap: int) -> None:
    # Inspect immutable ID before mutation to refuse recreated or relabeled containers.
    current = inspect_id(entry["id"])
    if current["id"] != entry["id"] or current["service"] != entry["service"]:
        raise ToolError("A managed container changed identity; refusing a stale memory update.")
    docker_update(entry["id"], memory, memory_swap)
    current = inspect_id(entry["id"])
    if current["memory"] != memory or current["memory_swap"] != memory_swap:
        raise ToolError(f"Docker did not apply the expected limits for {entry['service']}.")


def apply(caps: dict[str, int], path: Path) -> dict[str, Any]:
    if path.exists():
        raise ToolError(f"A restore point already exists at {path}; restore it before applying again.")
    found = discover(caps)
    if not found:
        raise ToolError("No containers from the fixed Compose project were found.")
    converter = next((item for item in found if item["service"] == "subtitle-converter"), None)
    if converter and converter_ocr_enabled(converter["id"]) and caps["subtitle-converter"] < OCR_MINIMUM_MEMORY:
        raise ToolError("OCR is enabled in the converter; its declared memory limit must be at least 1536 MiB.")
    snapshot = [dict(item) for item in found]
    write_snapshot(path, snapshot)
    attempted: list[dict[str, Any]] = []
    try:
        for item in snapshot:
            memory = caps[item["service"]]
            attempted.append(item)
            verify_id_and_update(item, memory, swap_target(item["memory"], item["memory_swap"], memory))
    except ToolError:
        failures = []
        for item in reversed(attempted):
            try:
                verify_id_and_update(item, item["memory"], item["memory_swap"])
            except ToolError:
                failures.append(item["service"])
        if failures:
            raise ToolError("Apply failed and rollback was incomplete; restore point retained for: " + ", ".join(failures)) from None
        path.unlink(missing_ok=True)
        raise
    return {"action": "applied", "services": [entry["service"] for entry in attempted], "restore_point": str(path)}


def restore(path: Path) -> dict[str, Any]:
    snapshot = read_snapshot(path)
    # Check every immutable ID and service label before changing any settings.
    for item in snapshot:
        current = inspect_id(item["id"])
        if current["id"] != item["id"] or current["service"] != item["service"]:
            raise ToolError(f"Container {item['service']} was recreated or relabeled; refusing stale restore.")
    restored = []
    for item in snapshot:
        verify_id_and_update(item, item["memory"], item["memory_swap"])
        restored.append(item["service"])
    path.unlink()
    return {"action": "restored", "services": restored}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group()
    action.add_argument("--apply", action="store_true", help="apply Compose-default caps and save restore point")
    action.add_argument("--restore", action="store_true", help="restore the saved memory and swap limits")
    parser.add_argument("--state-file", type=Path, default=STATE_PATH)
    args = parser.parse_args(argv)
    try:
        caps = compose_caps()
        if args.apply:
            result = apply(caps, args.state_file)
        elif args.restore:
            result = restore(args.state_file)
        else:
            result = plan(caps, discover(caps))
        print(json.dumps(result, indent=2))
        return 0
    except ToolError as error:
        print(json.dumps({"error": str(error)}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
