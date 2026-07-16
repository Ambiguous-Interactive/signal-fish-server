#!/usr/bin/env bash
# Validate repository-scoped Agent Skills packages.

set -euo pipefail

SKILLS_ROOT=".agents/skills"
MAX_SKILL_LINES=500
ERRORS=0
CHECKED=0

error() {
    printf '[ERROR] %s\n' "$1"
    ERRORS=$((ERRORS + 1))
}

info() {
    printf '[INFO] %s\n' "$1"
}

usage() {
    cat <<'EOF'
Usage:
  ./scripts/validate-agent-skills.sh
  ./scripts/validate-agent-skills.sh --files <changed-path>...
EOF
}

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || printf '.')
cd "$REPO_ROOT"

declare -a SKILL_DIRS=()

add_skill_dir() {
    local candidate=$1
    local existing
    for existing in "${SKILL_DIRS[@]:-}"; do
        [ "$existing" = "$candidate" ] && return
    done
    SKILL_DIRS+=("$candidate")
}

if [ "${1:-}" = "--files" ]; then
    shift
    if [ "$#" -eq 0 ]; then
        printf '[ERROR] --files requires at least one path\n' >&2
        exit 2
    fi
    for path in "$@"; do
        case "$path" in
            .agents/skills/*/*)
                relative=${path#"$SKILLS_ROOT/"}
                add_skill_dir "$SKILLS_ROOT/${relative%%/*}"
                ;;
        esac
    done
else
    while IFS= read -r skill_file; do
        add_skill_dir "${skill_file%/SKILL.md}"
    done < <(find "$SKILLS_ROOT" -mindepth 2 -maxdepth 2 -type f -name SKILL.md | LC_ALL=C sort)
fi

if [ ! -d "$SKILLS_ROOT" ]; then
    error "Missing repository skill root: $SKILLS_ROOT"
fi

for skill_dir in "${SKILL_DIRS[@]:-}"; do
    [ -n "$skill_dir" ] || continue
    CHECKED=$((CHECKED + 1))
    skill_file="$skill_dir/SKILL.md"
    metadata_file="$skill_dir/agents/openai.yaml"
    directory_name=${skill_dir##*/}

    if [[ ! "$directory_name" =~ ^[a-z0-9]+(-[a-z0-9]+)*$ ]] || [ "${#directory_name}" -gt 64 ]; then
        error "$skill_dir: directory name must be lowercase hyphen-case and at most 64 characters"
    fi

    if [ ! -f "$skill_file" ]; then
        error "$skill_dir: missing SKILL.md"
        continue
    fi

    frontmatter=$(awk '
        NR == 1 && $0 == "---" { inside=1; next }
        inside && $0 == "---" { exit }
        inside { print }
    ' "$skill_file")
    frontmatter_count=$(printf '%s\n' "$frontmatter" | awk 'NF { count++ } END { print count + 0 }')

    if [ "$(sed -n '1p' "$skill_file")" != "---" ] || [ "$frontmatter_count" -ne 2 ]; then
        error "$skill_file: frontmatter must contain exactly name and description"
    fi

    name=$(printf '%s\n' "$frontmatter" | sed -n 's/^name:[[:space:]]*//p' | tr -d '"' | sed "s/'//g")
    description=$(printf '%s\n' "$frontmatter" | sed -n 's/^description:[[:space:]]*//p')
    keys=$(printf '%s\n' "$frontmatter" | sed -n 's/^\([a-zA-Z0-9_-]*\):.*/\1/p' | LC_ALL=C sort | tr '\n' ' ')

    [ "$keys" = "description name " ] || error "$skill_file: only name and description frontmatter keys are allowed"
    [ "$name" = "$directory_name" ] || error "$skill_file: name '$name' must match directory '$directory_name'"
    [ -n "$description" ] || error "$skill_file: description must be non-empty"

    line_count=$(awk 'END { print NR }' "$skill_file")
    [ "$line_count" -le "$MAX_SKILL_LINES" ] || error "$skill_file: $line_count lines exceeds $MAX_SKILL_LINES"
    if rg -q 'TODO|Structuring This Skill' "$skill_file"; then
        error "$skill_file: scaffold placeholders remain"
    fi

    if [ ! -f "$metadata_file" ]; then
        error "$skill_dir: missing agents/openai.yaml"
    else
        default_prompt=$(sed -n 's/^[[:space:]]*default_prompt:[[:space:]]*"\(.*\)"[[:space:]]*$/\1/p' "$metadata_file")
        short_description=$(sed -n 's/^[[:space:]]*short_description:[[:space:]]*"\(.*\)"[[:space:]]*$/\1/p' "$metadata_file")
        rg -q '^[[:space:]]*display_name:[[:space:]]*"[^"]+"' "$metadata_file" || error "$metadata_file: missing quoted display_name"
        [ "${#short_description}" -ge 25 ] && [ "${#short_description}" -le 64 ] || error "$metadata_file: short_description must be 25-64 characters"
        [[ "$default_prompt" == *"\$$name"* ]] || error "$metadata_file: default_prompt must mention \$$name"
    fi

    if [ -d "$skill_dir/references" ]; then
        while IFS= read -r reference; do
            basename=${reference##*/}
            rg -Fq "references/$basename" "$skill_file" || error "$skill_file: does not directly route reference $basename"
        done < <(find "$skill_dir/references" -maxdepth 1 -type f | LC_ALL=C sort)
    fi

    while IFS= read -r target; do
        [ -e "$skill_dir/$target" ] || error "$skill_file: broken local resource link $target"
    done < <(sed -n 's/.*](\(\(references\|scripts\|assets\)\/[^)#]*\).*/\1/p' "$skill_file")
done

if [ "$CHECKED" -eq 0 ] && [ "$ERRORS" -eq 0 ]; then
    error "No SKILL.md packages found under $SKILLS_ROOT"
fi

if [ "$ERRORS" -gt 0 ]; then
    printf '[ERROR] Agent Skills validation found %d problem(s) across %d package(s)\n' "$ERRORS" "$CHECKED"
    exit 1
fi

info "Validated $CHECKED Agent Skills package(s)"
printf '[OK] Agent Skills structure and routing are valid\n'
