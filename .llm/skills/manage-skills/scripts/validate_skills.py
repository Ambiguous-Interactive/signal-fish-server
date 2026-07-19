#!/usr/bin/env python3
"""Validate the repository's portable Agent Skills library without third-party packages."""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path


SKILLS_DIR = Path(__file__).resolve().parents[2]
LLM_DIR = SKILLS_DIR.parent
NAME_PATTERN = re.compile(r"^[a-z0-9]+(?:-[a-z0-9]+)*$")
MARKDOWN_LINK_PATTERN = re.compile(r"\[[^]]+\]\(([^)]+)\)")
MAX_MARKDOWN_LINES = 300


@dataclass(frozen=True)
class SkillMetadata:
    name: str
    description: str
    title: str
    path: Path


def parse_skill(path: Path) -> SkillMetadata:
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()
    if not lines or lines[0] != "---":
        raise ValueError("SKILL.md must start with YAML frontmatter")

    try:
        closing = lines.index("---", 1)
    except ValueError as error:
        raise ValueError("frontmatter is missing its closing delimiter") from error

    frontmatter = lines[1:closing]
    keys = [match.group(1) for line in frontmatter if (match := re.match(r"^([a-z-]+):", line))]
    if keys != ["name", "description"]:
        raise ValueError("frontmatter must contain exactly `name` then `description`")

    name_line = next(line for line in frontmatter if line.startswith("name:"))
    name = name_line.removeprefix("name:").strip()
    description_line = next(line for line in frontmatter if line.startswith("description:"))
    description_value = description_line.removeprefix("description:").strip()
    description_start = frontmatter.index(description_line) + 1
    if description_value in {">", ">-", "|", "|-"}:
        description = " ".join(line.strip() for line in frontmatter[description_start:]).strip()
    else:
        description = description_value.strip('"\'')

    if not NAME_PATTERN.fullmatch(name) or len(name) > 64:
        raise ValueError("name must be 1-64 lowercase letters, digits, or hyphen-separated words")
    if name != path.parent.name:
        raise ValueError(f"name `{name}` must match folder `{path.parent.name}`")
    if not description or len(description) > 1024:
        raise ValueError("description must contain 1-1024 characters")
    if "use " not in description.lower():
        raise ValueError("description must state when to use the skill")

    body = lines[closing + 1 :]
    title = next((line.removeprefix("# ").strip() for line in body if line.startswith("# ")), "")
    if not title:
        raise ValueError("SKILL.md body must contain a top-level title")
    return SkillMetadata(name=name, description=description, title=title, path=path)


def validate_markdown_links(path: Path) -> list[str]:
    errors: list[str] = []
    visible_lines: list[str] = []
    in_fence = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if re.match(r"^\s*(```|~~~)", line):
            in_fence = not in_fence
            continue
        if not in_fence:
            visible_lines.append(re.sub(r"`[^`]*`", "", line))
    text = "\n".join(visible_lines)
    for raw_target in MARKDOWN_LINK_PATTERN.findall(text):
        target = raw_target.strip().strip("<>").split(maxsplit=1)[0]
        if target.startswith(("#", "http://", "https://", "mailto:")):
            continue
        file_target = target.split("#", 1)[0].split("?", 1)[0]
        if file_target and not (path.parent / file_target).resolve().exists():
            errors.append(f"{path.relative_to(LLM_DIR.parent)}: missing link target `{file_target}`")
    return errors


def validate_library() -> tuple[list[SkillMetadata], list[str]]:
    errors: list[str] = []
    flat_markdown = [path for path in SKILLS_DIR.glob("*.md") if path.name != "index.md"]
    for path in flat_markdown:
        errors.append(f"{path.relative_to(LLM_DIR.parent)}: flat skill files are not allowed")

    metadata: list[SkillMetadata] = []
    for directory in sorted(path for path in SKILLS_DIR.iterdir() if path.is_dir()):
        entrypoint = directory / "SKILL.md"
        if not entrypoint.is_file():
            errors.append(f"{directory.relative_to(LLM_DIR.parent)}: missing SKILL.md")
            continue
        try:
            skill = parse_skill(entrypoint)
            metadata.append(skill)
        except ValueError as error:
            errors.append(f"{entrypoint.relative_to(LLM_DIR.parent)}: {error}")
            continue

        body = entrypoint.read_text(encoding="utf-8")
        for resource_dir_name in ("references", "scripts", "assets"):
            resource_dir = directory / resource_dir_name
            if not resource_dir.is_dir():
                continue
            for resource in sorted(path for path in resource_dir.rglob("*") if path.is_file()):
                relative = resource.relative_to(directory).as_posix()
                if relative not in body:
                    errors.append(
                        f"{resource.relative_to(LLM_DIR.parent)}: resource is not linked directly from SKILL.md"
                    )

    for markdown in sorted(LLM_DIR.rglob("*.md")):
        line_count = len(markdown.read_text(encoding="utf-8").splitlines())
        if markdown != SKILLS_DIR / "index.md" and line_count > MAX_MARKDOWN_LINES:
            errors.append(
                f"{markdown.relative_to(LLM_DIR.parent)}: {line_count} lines exceeds {MAX_MARKDOWN_LINES}"
            )
        errors.extend(validate_markdown_links(markdown))

    return metadata, errors


def main() -> int:
    metadata, errors = validate_library()
    if errors:
        for error in errors:
            print(f"ERROR: {error}", file=sys.stderr)
        print(f"Skill validation failed with {len(errors)} error(s).", file=sys.stderr)
        return 1

    reference_count = sum(1 for path in SKILLS_DIR.glob("*/references/**/*") if path.is_file())
    print(f"Validated {len(metadata)} skills and {reference_count} bundled reference files.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
