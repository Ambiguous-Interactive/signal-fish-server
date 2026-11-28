#!/usr/bin/env bash
# read-toml-string.sh - Read a simple TOML string or scalar value.
#
# Usage:
#   bash scripts/read-toml-string.sh <file> <key> [section]
#
# This entrypoint stays self-contained so release scratch directories can copy
# one file. It needs no Rust or taplo install. The parser is Python's standard
# library TOML reader, which decodes every TOML escape exactly; the shell keeps
# the original usage and missing-file errors.

set -euo pipefail

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    echo "Usage: $0 <file> <key> [section]" >&2
    exit 2
fi

FILE="$1"
KEY="$2"
SECTION="${3:-}"

if [ ! -f "$FILE" ]; then
    echo "ERROR: TOML file not found: $FILE" >&2
    exit 2
fi

for python in python3 python; do
    if command -v "$python" >/dev/null 2>&1 && "$python" -c 'import tomllib' >/dev/null 2>&1; then
        exec "$python" - "$FILE" "$KEY" "$SECTION" <<'PY'
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


def _section_table(data: dict[str, Any], section: str) -> dict[str, Any]:
    literal = data.get(section)
    if isinstance(literal, dict):
        return literal

    table: Any = data
    for part in section.split("."):
        if not isinstance(table, dict) or part not in table:
            raise KeyError(section)
        table = table[part]
    if not isinstance(table, dict):
        raise KeyError(section)
    return table


def _lookup(data: dict[str, Any], key: str, section: str | None) -> Any:
    table = data if section is None else _section_table(data, section)
    if key not in table:
        raise KeyError(key)
    return table[key]


def main(argv: list[str]) -> int:
    if len(argv) < 3 or len(argv) > 4:
        print("Usage: read_toml_string.py <file> <key> [section]", file=sys.stderr)
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
    except (OSError, tomllib.TOMLDecodeError) as error:
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
PY
    fi
done

echo "ERROR: Python 3.11 or newer is required to read TOML values." >&2
exit 2
