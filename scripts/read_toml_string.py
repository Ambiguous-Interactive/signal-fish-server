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


def _tables(value: Any) -> list[dict[str, Any]]:
    if isinstance(value, dict):
        return [value]
    # TOML parses [[table]] as a list of tables. Search every entry in order:
    # the previous reader returned the first matching header or key.
    if isinstance(value, list) and value and all(isinstance(item, dict) for item in value):
        return list(value)
    return []


def _resolve_sections(value: Any, parts: list[str]) -> list[dict[str, Any]]:
    if not parts:
        return _tables(value)
    part = parts[0]
    if isinstance(value, dict):
        if part in value:
            return _resolve_sections(value[part], parts[1:])
        return []
    if isinstance(value, list):
        resolved: list[dict[str, Any]] = []
        for item in value:
            resolved.extend(_resolve_sections(item, parts))
        return resolved
    return []


def _section_tables(data: dict[str, Any], section: str) -> list[dict[str, Any]]:
    literal = data.get(section)
    if literal is not None:
        tables = _tables(literal)
        if tables:
            return tables
    return _resolve_sections(data, section.split("."))


def _lookup(data: dict[str, Any], key: str, section: str | None) -> Any:
    tables = [data] if section is None else _section_tables(data, section)
    # An array-of-tables key may live in any entry. The previous line-oriented
    # reader returned the first match, so search candidates in order.
    for table in tables:
        if key in table:
            return table[key]
    raise KeyError(key)


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
