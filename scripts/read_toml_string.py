#!/usr/bin/env python3
"""Read one TOML scalar value for CI bootstrap steps.

Usage:
    python3 scripts/read_toml_string.py <file> <key> [section]

The reader uses the Python standard library ``tomllib`` so escaped quotes,
backslashes, and Unicode escapes decode exactly. It prints one decoded scalar.
Exit 1 means the key or section is absent; exit 3 means the file is not valid
TOML, is not valid UTF-8, or the selected value is not a scalar. The shell
entrypoint keeps the public ``read-toml-string.sh`` interface and its usage
and missing-file errors.
"""

from __future__ import annotations

import datetime
import sys
import tomllib
from pathlib import Path
from typing import Any


def _format_scalar(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (str, int, float)):
        return str(value)
    if isinstance(value, (datetime.date, datetime.time, datetime.datetime)):
        return value.isoformat()
    raise TypeError(f"expected a TOML scalar, found {type(value).__name__}")


def _as_table(value: Any) -> dict[str, Any] | None:
    if isinstance(value, dict):
        return value
    # TOML parses [[table]] as a list of tables. A direct section lookup keeps
    # the previous reader's first-table behavior.
    if isinstance(value, list) and value and all(isinstance(item, dict) for item in value):
        return value[0]
    return None


def _resolve_section(value: Any, parts: list[str]) -> dict[str, Any] | None:
    if not parts:
        return _as_table(value)
    part = parts[0]
    if isinstance(value, dict):
        if part in value:
            resolved = _resolve_section(value[part], parts[1:])
            if resolved is not None:
                return resolved
        return None
    if isinstance(value, list):
        # A nested section attaches to the last array-table entry in TOML,
        # but the previous reader returned the first matching header. Search
        # entries in order for the first one that contains the nested table.
        for item in value:
            resolved = _resolve_section(item, parts)
            if resolved is not None:
                return resolved
    return None


def _section_table(data: dict[str, Any], section: str) -> dict[str, Any]:
    literal = _as_table(data.get(section))
    if literal is not None:
        return literal

    resolved = _resolve_section(data, section.split("."))
    if resolved is None:
        raise KeyError(section)
    return resolved


def _lookup(data: dict[str, Any], key: str, section: str | None) -> Any:
    table = data if section is None else _section_table(data, section)
    if key not in table:
        raise KeyError(key)
    return table[key]


def main(argv: list[str]) -> int:
    if len(argv) < 3 or len(argv) > 4:
        print("Usage: read-toml-string.sh <file> <key> [section]", file=sys.stderr)
        return 2

    file_name = argv[1]
    key = argv[2]
    section = argv[3] if len(argv) == 4 and argv[3] else None
    path = Path(file_name)
    if not path.is_file():
        print(f"ERROR: TOML file not found: {file_name}", file=sys.stderr)
        return 2

    try:
        with path.open("rb") as handle:
            data = tomllib.load(handle)
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        print(f"ERROR: Could not read TOML file {file_name}: {error}", file=sys.stderr)
        return 3

    try:
        value = _lookup(data, key, section)
    except KeyError:
        return 1

    try:
        rendered = _format_scalar(value)
    except TypeError as error:
        print(f"ERROR: TOML value is not a scalar: {error}", file=sys.stderr)
        return 3

    print(rendered)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
