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


def _header_content(line: str) -> tuple[str, bool] | None:
    stripped = line.lstrip()
    if not stripped.startswith("["):
        return None
    array = stripped.startswith("[[")
    open_len = 2 if array else 1
    quote: str | None = None
    escaped = False
    index = open_len
    while index < len(stripped):
        char = stripped[index]
        if escaped:
            escaped = False
        elif quote is not None:
            if quote == '"' and char == "\\":
                escaped = True
            elif char == quote:
                quote = None
        elif char in "\"'":
            quote = char
        elif char == "]":
            if array:
                if stripped.startswith("]]", index):
                    content = stripped[open_len:index]
                    after = index + 2
                    break
            else:
                content = stripped[open_len:index]
                after = index + 1
                break
        index += 1
    else:
        return None
    rest = stripped[after:].strip()
    if rest and not rest.startswith("#"):
        return None
    return content, array


def _decode_key(raw: str) -> str:
    raw = raw.strip()
    if len(raw) >= 2 and raw[0] == raw[-1] and raw[0] in "\"'":
        try:
            return tomllib.loads(f"key = {raw}")["key"]
        except (tomllib.TOMLDecodeError, KeyError):
            return raw[1:-1]
    return raw


def _split_section(section: str) -> list[str]:
    raw_parts: list[str] = []
    current: list[str] = []
    quote: str | None = None
    escaped = False
    for char in section:
        if escaped:
            current.append(char)
            escaped = False
            continue
        if quote is not None:
            current.append(char)
            if quote == '"' and char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            continue
        if char in "\"'":
            quote = char
            current.append(char)
        elif char == ".":
            part = "".join(current).strip()
            if part:
                raw_parts.append(part)
            current = []
        else:
            current.append(char)
    part = "".join(current).strip()
    if part:
        raw_parts.append(part)
    return [_decode_key(part) for part in raw_parts]


def _canonical(parts: list[str]) -> str:
    return "\0".join(parts)


def _multiline_close(line: str, delimiter: str) -> int | None:
    if delimiter == "'''":
        return line.find("'''")
    index = 0
    while True:
        index = line.find('"""', index)
        if index == -1:
            return None
        backslashes = 0
        probe = index - 1
        while probe >= 0 and line[probe] == "\\":
            backslashes += 1
            probe -= 1
        if backslashes % 2 == 0:
            return index
        index += 1


def _multiline_open(line: str) -> str | None:
    quote: str | None = None
    escaped = False
    index = 0
    while index < len(line):
        char = line[index]
        if escaped:
            escaped = False
            index += 1
            continue
        if quote is not None:
            if quote == '"' and char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            index += 1
            continue
        if line.startswith('"""', index):
            close = _multiline_close(line[index + 3 :], '"""')
            if close is None:
                return '"""'
            index = index + 3 + close + 3
            continue
        if line.startswith("'''", index):
            close = line.find("'''", index + 3)
            if close == -1:
                return "'''"
            index = close + 3
            continue
        if char in "\"'":
            quote = char
        elif char == "#":
            return None
        index += 1
    return None


def _declared_sections(text: str) -> dict[str, tuple[list[str], list[int]] | None]:
    declared: dict[str, tuple[list[str], list[int]] | None] = {}
    array_counts: dict[str, int] = {}
    multiline: str | None = None
    for line in text.splitlines():
        if multiline is not None:
            if _multiline_close(line, multiline) is not None:
                multiline = None
            continue
        header = _header_content(line)
        if header is not None:
            content, array = header
            parts = _split_section(content)
            if not parts:
                continue
            canonical = _canonical(parts)
            if array:
                array_counts[canonical] = array_counts.get(canonical, 0) + 1
                declared.setdefault(canonical, None)
            else:
                owner_prefix = None
                owner_index = None
                for length in range(len(parts) - 1, 0, -1):
                    prefix = parts[:length]
                    prefix_canonical = _canonical(prefix)
                    if prefix_canonical in array_counts:
                        owner_prefix = prefix
                        owner_index = array_counts[prefix_canonical] - 1
                        break
                if owner_prefix is None:
                    declared.setdefault(canonical, None)
                elif canonical not in declared:
                    declared[canonical] = (owner_prefix, [owner_index])
                else:
                    existing = declared[canonical]
                    if existing is not None and existing[0] == owner_prefix:
                        existing[1].append(owner_index)
            continue
        multiline = _multiline_open(line)
    return declared


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


def _section_tables(
    data: dict[str, Any],
    section: str,
    declared: dict[str, tuple[list[str], list[int]] | None],
) -> list[dict[str, Any]]:
    # A section exists only when the source declares a matching table header.
    # This keeps `items = [{ version = "..." }]` an array value, not a section.
    parts = _split_section(section)
    if not parts:
        return []
    canonical = _canonical(parts)
    if canonical not in declared:
        return []
    owner = declared[canonical]
    if owner is None:
        return _resolve_sections(data, parts)
    prefix, indices = owner
    entries = _resolve_sections(data, prefix)
    suffix = parts[len(prefix) :]
    tables: list[dict[str, Any]] = []
    for index in indices:
        if index < len(entries):
            tables.extend(_resolve_sections(entries[index], suffix))
    return tables


def _lookup(
    data: dict[str, Any],
    key: str,
    section: str | None,
    declared: dict[str, tuple[list[str], list[int]] | None],
) -> Any:
    tables = [data] if section is None else _section_tables(data, section, declared)
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
        text = path.read_text(encoding="utf-8")
        data = tomllib.loads(text)
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        print(f"ERROR: Could not read TOML file {file_name}: {error}", file=sys.stderr)
        return 3

    try:
        value = _lookup(data, key, section, _declared_sections(text))
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
