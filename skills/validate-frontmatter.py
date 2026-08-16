#!/usr/bin/env python3
"""Validate the deliberately small YAML schema used by KYB's SKILL.md."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path
from typing import NoReturn


NAME_RE = re.compile(r"[a-z0-9]+(?:-[a-z0-9]+)*")


def fail(message: str) -> NoReturn:
    raise ValueError(message)


def extract_frontmatter(path: Path) -> tuple[str, dict[str, str]]:
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    if not lines or lines[0] != "---":
        fail("frontmatter must start with an exact --- line")
    try:
        end = lines.index("---", 1)
    except ValueError:
        fail("frontmatter is missing its closing --- line")

    raw_lines = lines[1:end]
    if any("\t" in line for line in raw_lines):
        fail("tabs are not allowed in frontmatter")
    if len(raw_lines) < 3 or not raw_lines[0].startswith("name: "):
        fail("frontmatter must start with name: <skill-name>")

    name = raw_lines[0][len("name: ") :]
    if not NAME_RE.fullmatch(name) or len(name) > 64:
        fail("name must be lowercase hyphen-case and at most 64 characters")
    if raw_lines[1] != "description: >-":
        fail("description must use the folded block form: description: >-")

    description_lines = raw_lines[2:]
    if not description_lines or any(not line.startswith("  ") for line in description_lines):
        fail("every description line must be indented by two spaces")
    description = " ".join(line[2:].strip() for line in description_lines).strip()
    if not description:
        fail("description must not be empty")

    frontmatter = "\n".join(raw_lines) + "\n"
    return frontmatter, {"name": name, "description": description}


def parse_with_psych(frontmatter: str) -> dict[str, object] | None:
    ruby = shutil.which("ruby")
    if ruby is None:
        return None
    script = """
require "json"
require "yaml"
value = YAML.safe_load(STDIN.read, permitted_classes: [], aliases: false)
abort "frontmatter must decode to a mapping" unless value.is_a?(Hash)
puts JSON.generate(value)
"""
    result = subprocess.run(
        [ruby, "-e", script],
        input=frontmatter,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        fail(f"Ruby Psych rejected YAML frontmatter: {detail}")
    return json.loads(result.stdout)


def validate(path: Path, require_yaml_parser: bool) -> None:
    frontmatter, expected = extract_frontmatter(path)
    parsed = parse_with_psych(frontmatter)
    if parsed is None:
        if require_yaml_parser:
            fail("Ruby is required for strict YAML parser validation")
        return
    if parsed != expected:
        fail(f"parsed frontmatter does not match the required schema: {parsed!r}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("path", type=Path)
    parser.add_argument("--require-yaml-parser", action="store_true")
    args = parser.parse_args()
    try:
        validate(args.path, args.require_yaml_parser)
    except (OSError, UnicodeError, ValueError, json.JSONDecodeError) as error:
        print(f"invalid skill frontmatter: {args.path}: {error}", file=sys.stderr)
        return 1
    print(f"valid skill frontmatter: {args.path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
