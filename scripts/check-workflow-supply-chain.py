#!/usr/bin/env python3
"""Fail closed on mutable or over-privileged GitHub Actions workflows."""

from __future__ import annotations

import re
import stat
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


ALLOWED_RUNNERS = {
    "ubuntu-24.04",
    "macos-26",
    "${{ matrix.os }}",
}
EXACT_DESKTOP_MATRIX = "os: [ubuntu-24.04, windows-2025, macos-26]"
MAX_TIMEOUT_MINUTES = 60
MAX_WORKFLOW_BYTES = 1024 * 1024
PINNED_OFFICIAL_ACTIONS = {
    "actions/upload-artifact": "043fb46d1a93c77aae656e7c1c64a875d1fc6a0a",
    # Exact official commits already selected by this repository.
    "actions/checkout": "3d3c42e5aac5ba805825da76410c181273ba90b1",
    "actions/setup-node": "820762786026740c76f36085b0efc47a31fe5020",
}


class PolicyError(RuntimeError):
    """A workflow violates the repository's fail-closed policy."""


@dataclass(frozen=True)
class Line:
    number: int
    indent: int
    content: str


USES_KEY = re.compile(r"(?:^|[,{\s-])[\"']?uses[\"']?\s*:")
PERMISSIONS_KEY = re.compile(r"^[\"']?permissions[\"']?\s*:")
RUNNER_KEY = re.compile(r"^[\"']?runs-on[\"']?\s*:")
TIMEOUT_KEY = re.compile(r"^[\"']?timeout-minutes[\"']?\s*:")
PERSIST_CREDENTIALS_KEY = re.compile(r"^[\"']?persist-credentials[\"']?\s*:")
JOB_HEADER = re.compile(r"[A-Za-z0-9_-]+:")
PINNED_ACTION = re.compile(
    r"(?P<name>actions/[A-Za-z0-9_.-]+)@(?P<sha>[0-9a-f]{40})"
)
PINNED_SERVICE_IMAGE = re.compile(r"image:\s*[^\s@]+@sha256:[0-9a-f]{64}")


def fail(source: str, line: Line | None, message: str) -> PolicyError:
    location = source if line is None else f"{source}:{line.number}"
    return PolicyError(f"{location}: {message}")


def logical_lines(source: str, text: str) -> list[Line]:
    if len(text.encode("utf-8")) > MAX_WORKFLOW_BYTES:
        raise fail(source, None, "workflow exceeds the 1 MiB policy limit")

    result: list[Line] = []
    for number, raw in enumerate(text.splitlines(), start=1):
        if "\t" in raw:
            raise fail(source, Line(number, 0, raw), "tabs are forbidden in workflows")
        # A comment begins only after whitespace here. This preserves hashes and
        # other value characters while accepting a human-readable action tag.
        uncommented = re.sub(r"\s+#.*$", "", raw).rstrip()
        if not uncommented.strip() or uncommented.lstrip().startswith("#"):
            continue
        indent = len(uncommented) - len(uncommented.lstrip(" "))
        content = uncommented[indent:]
        if content.startswith("<<:") or re.search(
            r"(?:^|[\s:{,\[])(?:&|\*)[A-Za-z0-9_-]+", content
        ):
            raise fail(
                source,
                Line(number, indent, content),
                "YAML anchors, aliases, and merge keys are forbidden",
            )
        result.append(Line(number, indent, content))
    return result


def one_exact_top_level_permissions(source: str, lines: list[Line]) -> None:
    candidates = [
        line
        for line in lines
        if line.indent == 0 and PERMISSIONS_KEY.match(line.content)
    ]
    if len(candidates) != 1 or candidates[0].content != "permissions: {}":
        raise fail(
            source,
            candidates[0] if candidates else None,
            "top-level permissions must appear exactly once as permissions: {}",
        )


def job_segments(source: str, lines: list[Line]) -> list[tuple[Line, list[Line]]]:
    jobs = [
        index
        for index, line in enumerate(lines)
        if line.indent == 0 and line.content == "jobs:"
    ]
    if len(jobs) != 1:
        raise fail(source, None, "workflow must contain exactly one block-style jobs mapping")

    start = jobs[0] + 1
    end = next(
        (index for index in range(start, len(lines)) if lines[index].indent == 0),
        len(lines),
    )
    body = lines[start:end]
    headers = [
        index
        for index, line in enumerate(body)
        if line.indent == 2 and JOB_HEADER.fullmatch(line.content)
    ]
    if not headers:
        raise fail(source, lines[jobs[0]], "jobs mapping must contain at least one job")
    for line in body:
        if line.indent == 2 and not JOB_HEADER.fullmatch(line.content):
            raise fail(source, line, "jobs must use an explicit block mapping")

    segments: list[tuple[Line, list[Line]]] = []
    for position, header_index in enumerate(headers):
        segment_end = headers[position + 1] if position + 1 < len(headers) else len(body)
        segments.append((body[header_index], body[header_index + 1 : segment_end]))
    return segments


def matching_job_property(segment: list[Line], pattern: re.Pattern[str]) -> list[Line]:
    return [
        line
        for line in segment
        if line.indent == 4 and pattern.match(line.content)
    ]


def validate_job(source: str, header: Line, segment: list[Line]) -> None:
    runners = matching_job_property(segment, RUNNER_KEY)
    runner = None
    if len(runners) == 1:
        match = re.fullmatch(r"runs-on:\s*(.+)", runners[0].content)
        if match is not None:
            runner = match.group(1)
    if runner not in ALLOWED_RUNNERS:
        raise fail(
            source,
            runners[0] if runners else header,
            "every job must use an allowlisted immutable runner",
        )
    desktop_matrices = [
        line for line in segment if line.content == EXACT_DESKTOP_MATRIX
    ]
    if runner == "${{ matrix.os }}" and len(desktop_matrices) != 1:
        raise fail(
            source,
            runners[0],
            "matrix.os must use the exact fixed desktop runner matrix",
        )
    if runner != "${{ matrix.os }}" and desktop_matrices:
        raise fail(source, desktop_matrices[0], "desktop runner matrix is unused")

    timeouts = matching_job_property(segment, TIMEOUT_KEY)
    if len(timeouts) != 1:
        raise fail(
            source,
            timeouts[0] if timeouts else header,
            "every job must declare exactly one timeout-minutes value",
        )
    match = re.fullmatch(r"timeout-minutes: ([0-9]+)", timeouts[0].content)
    if match is None:
        raise fail(source, timeouts[0], "timeout-minutes must be a literal integer")
    timeout = int(match.group(1))
    if not 1 <= timeout <= MAX_TIMEOUT_MINUTES:
        raise fail(
            source,
            timeouts[0],
            f"timeout-minutes must be between 1 and {MAX_TIMEOUT_MINUTES}",
        )

    permissions = matching_job_property(segment, PERMISSIONS_KEY)
    if len(permissions) != 1 or permissions[0].content != "permissions:":
        raise fail(
            source,
            permissions[0] if permissions else header,
            "every job must declare one block-style minimal permissions mapping",
        )
    permission_index = segment.index(permissions[0])
    permission_end = next(
        (
            index
            for index in range(permission_index + 1, len(segment))
            if segment[index].indent <= 4
        ),
        len(segment),
    )
    entries = segment[permission_index + 1 : permission_end]
    if len(entries) != 1 or entries[0].indent != 6 or entries[0].content != "contents: read":
        raise fail(
            source,
            entries[0] if entries else permissions[0],
            "job permissions must contain only contents: read",
        )


def validate_checkout_credentials(
    source: str, lines: list[Line], uses_index: int, uses_line: Line
) -> None:
    step_start = uses_index
    while step_start >= 0:
        candidate = lines[step_start]
        if candidate.indent <= uses_line.indent and candidate.content.startswith("- "):
            break
        step_start -= 1
    if step_start < 0:
        raise fail(source, uses_line, "checkout must appear in an explicit step")
    step_indent = lines[step_start].indent
    step_end = next(
        (
            index
            for index in range(step_start + 1, len(lines))
            if lines[index].indent == step_indent and lines[index].content.startswith("- ")
        ),
        len(lines),
    )
    step = lines[step_start:step_end]
    property_indent = uses_line.indent + (2 if uses_line.content.startswith("- ") else 0)
    with_blocks = [
        (index, line)
        for index, line in enumerate(step)
        if line.indent == property_indent and line.content == "with:"
    ]
    if len(with_blocks) != 1:
        raise fail(
            source,
            with_blocks[0][1] if with_blocks else uses_line,
            "actions/checkout must contain one explicit block-style with mapping",
        )
    with_index, with_line = with_blocks[0]
    with_end = next(
        (
            index
            for index in range(with_index + 1, len(step))
            if step[index].indent <= property_indent
        ),
        len(step),
    )
    credentials = [
        line
        for line in step
        if PERSIST_CREDENTIALS_KEY.match(line.content)
    ]
    expected_credentials = step[with_index + 1 : with_end]
    if (
        len(credentials) != 1
        or credentials[0].content != "persist-credentials: false"
        or credentials[0].indent != property_indent + 2
        or credentials[0] not in expected_credentials
    ):
        raise fail(
            source,
            credentials[0] if credentials else with_line,
            "actions/checkout with mapping must set persist-credentials: false exactly once",
        )


def validate_actions(source: str, lines: list[Line]) -> None:
    action_count = 0
    for index, line in enumerate(lines):
        if not USES_KEY.search(line.content):
            continue
        match = re.fullmatch(r"(?:-\s*)?uses:\s*(\S+)", line.content)
        if match is None:
            raise fail(source, line, "uses must be a simple literal action reference")
        reference = match.group(1)
        pinned = PINNED_ACTION.fullmatch(reference)
        if pinned is None:
            raise fail(
                source,
                line,
                "actions must use an allowlisted official action at a full 40-character commit SHA",
            )
        action_name = pinned.group("name")
        expected_sha = PINNED_OFFICIAL_ACTIONS.get(action_name)
        if expected_sha is None or pinned.group("sha") != expected_sha:
            raise fail(
                source,
                line,
                "action and commit SHA are not in the verified official allowlist",
            )
        action_count += 1
        if action_name == "actions/checkout":
            validate_checkout_credentials(source, lines, index, line)
    if action_count == 0:
        raise fail(source, None, "workflow must contain at least one pinned official action")


def validate_service_images(source: str, lines: list[Line]) -> None:
    for line in lines:
        if not line.content.startswith("image:"):
            continue
        if PINNED_SERVICE_IMAGE.fullmatch(line.content) is None:
            raise fail(
                source,
                line,
                "service images must use a literal name and full sha256 digest",
            )


def validate_workflow(source: str, text: str) -> None:
    lines = logical_lines(source, text)
    one_exact_top_level_permissions(source, lines)
    for header, segment in job_segments(source, lines):
        validate_job(source, header, segment)
    validate_actions(source, lines)
    validate_service_images(source, lines)


def repository_root() -> Path:
    completed = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    return Path(completed.stdout.strip())


def tracked_workflows(root: Path) -> list[Path]:
    completed = subprocess.run(
        ["git", "-C", str(root), "ls-files", "-z", "--", ".github/workflows"],
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    paths = [
        root / Path(raw.decode("utf-8"))
        for raw in completed.stdout.split(b"\0")
        if raw and raw.endswith((b".yml", b".yaml"))
    ]
    if not paths:
        raise PolicyError("no Git-tracked GitHub Actions workflows found")
    return paths


def check_repository(root: Path) -> None:
    for path in tracked_workflows(root):
        relative = path.relative_to(root)
        if relative.parent != Path(".github/workflows"):
            raise PolicyError(f"{relative}: nested workflow paths are forbidden")
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            raise PolicyError(f"{relative}: workflow must be one regular, unlinked file")
        validate_workflow(relative.as_posix(), path.read_text(encoding="utf-8"))
        print(f"workflow policy: passed {relative}")


def negative_self_tests() -> None:
    base = f"""name: fixture
permissions: {{}}
jobs:
  test:
    permissions:
      contents: read
    runs-on: ubuntu-24.04
    timeout-minutes: 10
    services:
      fixture:
        image: example.invalid/service:1@sha256:{'0' * 64}
    steps:
      - uses: actions/checkout@{PINNED_OFFICIAL_ACTIONS['actions/checkout']}
        with:
          persist-credentials: false
"""
    validate_workflow("positive-fixture.yml", base)
    matrix = base.replace(
        "    runs-on: ubuntu-24.04\n",
        "    strategy:\n"
        "      matrix:\n"
        f"        {EXACT_DESKTOP_MATRIX}\n"
        "    runs-on: ${{ matrix.os }}\n",
    )
    validate_workflow("positive-matrix-fixture.yml", matrix)
    cases = {
        "floating action": base.replace(
            PINNED_OFFICIAL_ACTIONS["actions/checkout"], "v4"
        ),
        "floating runner": base.replace("ubuntu-24.04", "ubuntu-latest"),
        "over-broad permissions": base.replace("contents: read", "contents: write"),
        "persisted checkout credentials": base.replace(
            "persist-credentials: false", "persist-credentials: true"
        ),
        "unpinned service image": base.replace("@sha256:" + "0" * 64, ":latest"),
        "floating desktop matrix": matrix.replace("windows-2025", "windows-latest"),
    }
    for label, fixture in cases.items():
        try:
            validate_workflow(f"negative-{label}.yml", fixture)
        except PolicyError:
            continue
        raise PolicyError(f"negative self-test unexpectedly accepted {label}")
    print(
        "workflow policy negative tests: passed "
        "action/runner/matrix/permissions/credentials/service-image cases"
    )


def main() -> int:
    try:
        if sys.argv[1:] == ["--self-test-only"]:
            negative_self_tests()
            return 0
        if sys.argv[1:]:
            raise PolicyError("usage: check-workflow-supply-chain.py [--self-test-only]")
        check_repository(repository_root())
        negative_self_tests()
        return 0
    except (OSError, subprocess.SubprocessError, UnicodeError, PolicyError) as error:
        print(f"workflow policy: FAILED: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
