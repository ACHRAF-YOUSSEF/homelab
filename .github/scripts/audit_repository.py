#!/usr/bin/env python3
"""Check repository privacy invariants without displaying matched values."""

from __future__ import annotations

import argparse
import ipaddress
import re
import subprocess
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True, order=True)
class Finding:
    path: str
    line: int
    rule: str

    def __str__(self) -> str:
        return f"{self.path}:{self.line}: {self.rule}"


IPV4 = re.compile(r"(?<![\w.])(?:\d{1,3}\.){3}\d{1,3}(?![\w.])")
IPV6 = re.compile(r"(?<![\w])(?:[a-fA-F0-9]{0,4}:){2,}[a-fA-F0-9:.]*(?![\w])")
PRIVATE_NETWORKS = tuple(
    ipaddress.ip_network(network)
    for network in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16")
)
PRIVATE_KEY = re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----")
ASSIGNMENT = re.compile(r"(?<![\w-])(?P<name>[A-Za-z_][\w-]*)[\"']?\s*(?:=|:)\s*")
CREDENTIAL_NAME = re.compile(
    r"PASSWORD|PASSPHRASE|(?:^|_)TOKEN(?:_|$)|(?:^|_)API_?KEY(?:_|$)|"
    r"(?:^|_)SECRET(?:_|$)|PRIVATE_KEY|USERNAME|ADMIN_USER|^AUTHORIZATION$",
    re.IGNORECASE,
)
ENV_ASSIGNMENT = re.compile(r"^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=(.*)$")
URL_LITERAL = re.compile(r"\b[a-z][a-z0-9+.-]*://", re.IGNORECASE)
URL_FIELD = re.compile(
    r"(?:^\s*(?:-\s+)?|[,{]\s*)(?P<quote>[\"']?)"
    r"(?P<name>url|title-url|check-url|search-engine|sock-path)(?P=quote):\s*"
)
HREF = re.compile(r"\bhref\s*=\s*([\"'])(.*?)\1", re.IGNORECASE)
ENV_PREFIX = re.compile(r"^\$\{[A-Z][A-Z0-9_]*\}")


def literal_value(value: str) -> str:
    """Extract a quoted or unquoted assignment value, including inline Markdown."""
    value = value.strip()
    if not value:
        return ""
    if value[0] in "\"'":
        closing = value.find(value[0], 1)
        return value[1:closing] if closing >= 0 else value[1:]
    return re.split(r"[`;]|\s+#", value, maxsplit=1)[0].strip()


def placeholder(value: str) -> bool:
    if not value:
        return True
    if value.lower().startswith("bearer "):
        value = value[7:].strip()
    if re.fullmatch(r"\$\{[A-Za-z_][A-Za-z0-9_]*\}|\$(?:env:)?[A-Za-z_][A-Za-z0-9_]*|%[A-Za-z_][A-Za-z0-9_]*%", value):
        return True
    if re.fullmatch(r"<[^>]+>", value):
        return True
    return bool(
        re.fullmatch(
            r"(?:your|replace|example|test)[_-][A-Za-z0-9_-]+|"
            r"change[_-]?me|redacted|dummy|unused|not[_-]needed|lm[_-]?studio|none|x{3,}|\.\.\.",
            value,
            re.IGNORECASE,
        )
    )


def valid_addresses(line: str):
    for pattern in (IPV4, IPV6):
        for match in pattern.finditer(line):
            try:
                yield ipaddress.ip_address(match.group())
            except ValueError:
                continue


def audit_markdown(path: str, text: str) -> list[Finding]:
    findings = []
    for number, line in enumerate(text.splitlines(), 1):
        if any(address.version == 4 and any(address in network for network in PRIVATE_NETWORKS) for address in valid_addresses(line)):
            findings.append(Finding(path, number, "markdown-private-ip"))
        if PRIVATE_KEY.search(line):
            findings.append(Finding(path, number, "markdown-private-key"))
        for match in ASSIGNMENT.finditer(line):
            name = match["name"].replace("-", "_")
            if CREDENTIAL_NAME.search(name) and not placeholder(literal_value(line[match.end():])):
                findings.append(Finding(path, number, "markdown-credential-assignment"))
                break
    return findings


def audit_env_example(text: str) -> list[Finding]:
    findings = []
    docker_host_line = 0
    for number, line in enumerate(text.splitlines(), 1):
        match = ENV_ASSIGNMENT.match(line)
        if not match:
            continue
        name, raw_value = match.groups()
        value = literal_value(raw_value)
        if CREDENTIAL_NAME.search(name) and value:
            findings.append(Finding(".env.example", number, "example-credential-not-blank"))
        if name == "GLANCE_DOCKER_HOST":
            docker_host_line = number
            if value != "tcp://docker-proxy:2375":
                findings.append(Finding(".env.example", number, "example-docker-client-not-proxy"))
    if not docker_host_line:
        findings.append(Finding(".env.example", 0, "example-docker-proxy-setting-missing"))
    return findings


def relative_asset(value: str) -> bool:
    return bool(re.match(r"^(?:/|\./|\.\./)?assets/", value))


def audit_glance_yaml(path: str, text: str) -> list[Finding]:
    findings = []
    for number, line in enumerate(text.splitlines(), 1):
        if URL_LITERAL.search(line):
            findings.append(Finding(path, number, "glance-literal-url"))
        if any(valid_addresses(line)):
            findings.append(Finding(path, number, "glance-literal-ip"))
        for field in URL_FIELD.finditer(line):
            value = literal_value(line[field.end():])
            if field["name"] == "sock-path":
                if not re.fullmatch(r"\$\{GLANCE_DOCKER_HOST\}(?:\s*[,}].*)?", value):
                    findings.append(Finding(path, number, "glance-docker-client-not-env-proxy"))
            elif not (ENV_PREFIX.match(value) or relative_asset(value)):
                findings.append(Finding(path, number, "glance-endpoint-not-env"))
        for match in HREF.finditer(line):
            if not (ENV_PREFIX.match(match[2]) or relative_asset(match[2])):
                findings.append(Finding(path, number, "glance-href-not-env"))
    return findings


def git(root: Path, *args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", "-C", str(root), *args], capture_output=True, check=False
    )


def audit_repository(root: Path) -> list[Finding]:
    manifest = git(root, "ls-files", "--cached", "-z")
    untracked = git(root, "ls-files", "--others", "--exclude-standard", "-z")
    if manifest.returncode or untracked.returncode:
        raise RuntimeError("Cannot read Git manifest")
    tracked = set(filter(None, manifest.stdout.decode("utf-8").split("\0")))
    files = tracked | set(filter(None, untracked.stdout.decode("utf-8").split("\0")))
    findings = []
    for name in sorted(files):
        path = root / name
        if path.name == ".env" and name in tracked:
            findings.append(Finding(name, 0, "real-env-tracked"))
        if path.suffix.lower() == ".md" and path.is_file():
            findings.extend(audit_markdown(name, path.read_text(encoding="utf-8-sig")))
    ignored = git(root, "check-ignore", "--quiet", "--no-index", "--", ".env")
    if ignored.returncode == 1:
        findings.append(Finding(".gitignore", 0, "real-env-not-ignored"))
    elif ignored.returncode:
        raise RuntimeError("Cannot check Git ignore rules")
    example = root / ".env.example"
    if example.is_file():
        findings.extend(audit_env_example(example.read_text(encoding="utf-8-sig")))
    else:
        findings.append(Finding(".env.example", 0, "example-env-missing"))
    config = root / "glance" / "config"
    for path in sorted(config.rglob("*")):
        if path.is_file() and path.suffix.lower() in {".yml", ".yaml"}:
            name = path.relative_to(root).as_posix()
            findings.extend(audit_glance_yaml(name, path.read_text(encoding="utf-8-sig")))
    return sorted(set(findings))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args(argv)
    try:
        findings = audit_repository(args.root.resolve())
    except (OSError, UnicodeError, RuntimeError):
        print("Repository privacy audit could not read the repository.")
        return 2
    for finding in findings:
        print(finding)
    if findings:
        print(f"Repository privacy audit failed: {len(findings)} finding(s).")
        return 1
    print("Repository privacy audit passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
