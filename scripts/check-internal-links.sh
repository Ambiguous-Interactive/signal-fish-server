#!/usr/bin/env bash
# Validate internal Markdown links without network access.
#
# This checker complements lychee by producing concise file:line diagnostics and
# by failing links that point at untracked local files. The tracked-file check
# catches the class of failures where validation passes locally because an
# uncommitted file exists, but CI fails after a clean checkout.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$REPO_ROOT"

MODE="all"
QUIET="false"
CHECK_TRACKED="true"
FILES=()
TRACKED_PATHS_LOADED="false"
declare -A TRACKED_FILE_SET=()
declare -A TRACKED_DIR_SET=()

usage() {
    cat <<'EOF'
Usage: scripts/check-internal-links.sh [OPTIONS] [FILE...]

Options:
  --all                   Check all Markdown files (default)
  --docs-only             Check only docs/**/*.md
  --no-git-tracked-check  Only require local filesystem existence
  --quiet                 Suppress progress output
  -h, --help              Show this help
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --all)
            MODE="all"
            ;;
        --docs-only)
            MODE="docs"
            ;;
        --no-git-tracked-check)
            CHECK_TRACKED="false"
            ;;
        --quiet)
            QUIET="true"
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        --)
            shift
            while [ "$#" -gt 0 ]; do
                FILES+=("$1")
                shift
            done
            break
            ;;
        -*)
            echo "ERROR: unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
        *)
            FILES+=("$1")
            ;;
    esac
    shift
done

if [ "$CHECK_TRACKED" = "true" ] && ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    CHECK_TRACKED="false"
    if [ "$QUIET" = "false" ]; then
        echo "WARN: not in a Git worktree; skipping tracked-target checks"
    fi
fi

normalize_path() {
    local path="$1"
    local normalized

    if normalized=$(realpath -m -- "$path" 2>/dev/null); then
        printf '%s\n' "$normalized"
        return
    fi

    local dir base
    dir=$(dirname -- "$path")
    base=$(basename -- "$path")
    if [ -d "$dir" ]; then
        (cd "$dir" && printf '%s/%s\n' "$(pwd -P)" "$base")
    else
        printf '%s\n' "$path"
    fi
}

repo_relative_path() {
    local path="$1"
    local absolute
    absolute=$(normalize_path "$path")

    case "$absolute" in
        "$REPO_ROOT"/*)
            printf '%s\n' "${absolute#"$REPO_ROOT"/}"
            ;;
        "$REPO_ROOT")
            printf '.\n'
            ;;
        *)
            return 1
            ;;
    esac
}

load_tracked_paths() {
    [ "$TRACKED_PATHS_LOADED" = "true" ] && return

    local tracked_file dir parent
    while IFS= read -r -d '' tracked_file; do
        TRACKED_FILE_SET["$tracked_file"]=1

        dir=$(dirname -- "$tracked_file")
        while [ "$dir" != "." ] && [ "$dir" != "/" ] && [ -n "$dir" ]; do
            TRACKED_DIR_SET["$dir"]=1
            parent=$(dirname -- "$dir")
            [ "$parent" = "$dir" ] && break
            dir="$parent"
        done
    done < <(git ls-files -z --)

    TRACKED_PATHS_LOADED="true"
}

target_is_tracked() {
    local target="$1"
    local relative="$2"

    load_tracked_paths

    if [ -f "$target" ]; then
        [ -n "${TRACKED_FILE_SET[$relative]+tracked}" ]
        return
    fi

    if [ -d "$target" ]; then
        if [ "$relative" = "." ]; then
            [ "${#TRACKED_FILE_SET[@]}" -gt 0 ]
        else
            [ -n "${TRACKED_DIR_SET[$relative]+tracked}" ]
        fi
        return
    fi

    return 1
}

list_markdown_files() {
    if [ "${#FILES[@]}" -gt 0 ]; then
        printf '%s\0' "${FILES[@]}"
        return
    fi

    if [ "$CHECK_TRACKED" = "true" ]; then
        while IFS= read -r -d '' tracked_file; do
            case "$tracked_file" in
                target/*|third_party/*|.git/*|.github/test-fixtures/*|test-fixtures/*|node_modules/*|lychee/*)
                    continue
                    ;;
            esac
            case "$MODE:$tracked_file" in
                docs:docs/*|all:*)
                    printf '%s\0' "$tracked_file"
                    ;;
            esac
        done < <(git ls-files -z -- '*.md')
        return
    fi

    local root="."
    if [ "$MODE" = "docs" ]; then
        root="docs"
        [ -d "$root" ] || return
    fi

    find "$root" -type f -name "*.md" \
        -not -path "./target/*" \
        -not -path "./third_party/*" \
        -not -path "./.git/*" \
        -not -path "./.github/test-fixtures/*" \
        -not -path "./test-fixtures/*" \
        -not -path "./node_modules/*" \
        -not -path "./lychee/*" \
        -print0
}

extract_markdown_links() {
    local md_file="$1"
    perl - "$md_file" <<'PERL'
use strict;
use warnings;

my ($md_file) = @ARGV;
open my $fh, '<', $md_file or die "failed to read $md_file: $!\n";

my $in_fence = 0;
my $fence_char = '';
my $fence_width = 0;
my $line_num = 0;

while (my $line = <$fh>) {
    $line_num++;

    my $trimmed = $line;
    $trimmed =~ s/^[ \t]{0,3}//;

    if ($in_fence) {
        if ($trimmed =~ /^\Q$fence_char\E{$fence_width,}[ \t]*$/) {
            $in_fence = 0;
        }
        next;
    }

    if ($trimmed =~ /^(`{3,}|~{3,})/) {
        $in_fence = 1;
        $fence_char = substr($1, 0, 1);
        $fence_width = length($1);
        next;
    }

    $line = strip_inline_code($line);
    for my $target (extract_inline_link_targets($line)) {
        print "$line_num\t$target\n";
    }
}

sub strip_inline_code {
    my ($line) = @_;
    $line =~ s/`[^`]*`//g;
    return $line;
}

sub is_escaped {
    my ($s, $idx) = @_;
    my $slashes = 0;
    for (my $i = $idx - 1; $i >= 0 && substr($s, $i, 1) eq "\\"; $i--) {
        $slashes++;
    }
    return $slashes % 2 == 1;
}

sub find_closing_bracket {
    my ($s, $start) = @_;
    my $depth = 1;
    my $len = length($s);

    for (my $i = $start; $i < $len; $i++) {
        next if is_escaped($s, $i);
        my $ch = substr($s, $i, 1);
        if ($ch eq '[') {
            $depth++;
        } elsif ($ch eq ']') {
            $depth--;
            return $i if $depth == 0;
        }
    }

    return -1;
}

sub parse_parenthesized_link {
    my ($s, $start) = @_;
    my $depth = 0;
    my $quote = '';
    my $angle_destination = 0;
    my $quote_allowed = 0;
    my $len = length($s);

    for (my $i = $start; $i < $len; $i++) {
        next if is_escaped($s, $i);
        my $ch = substr($s, $i, 1);

        if ($quote ne '') {
            $quote = '' if $ch eq $quote;
            next;
        }

        if ($angle_destination) {
            $angle_destination = 0 if $ch eq '>';
            next;
        }

        if ($ch eq '<' && $depth == 0) {
            $angle_destination = 1;
            $quote_allowed = 0;
        } elsif ($quote_allowed && ($ch eq '"' || $ch eq "'")) {
            $quote = $ch;
            $quote_allowed = 0;
        } elsif ($ch =~ /\s/ && $depth == 0) {
            $quote_allowed = 1;
        } elsif ($ch eq '(') {
            $depth++;
            $quote_allowed = 0;
        } elsif ($ch eq ')') {
            return (substr($s, $start, $i - $start), $i) if $depth == 0;
            $depth--;
            $quote_allowed = 0;
        } elsif ($ch !~ /\s/) {
            $quote_allowed = 0;
        }
    }

    return;
}

sub normalize_link_target {
    my ($raw) = @_;
    $raw =~ s/^\s+|\s+$//g;
    return '' if $raw eq '';

    my $target = '';
    if (substr($raw, 0, 1) eq '<') {
        for (my $i = 1; $i < length($raw); $i++) {
            next if is_escaped($raw, $i);
            if (substr($raw, $i, 1) eq '>') {
                $target = substr($raw, 1, $i - 1);
                last;
            }
        }
    } else {
        my $depth = 0;
        for (my $i = 0; $i < length($raw); $i++) {
            my $ch = substr($raw, $i, 1);
            if (!is_escaped($raw, $i)) {
                last if $ch =~ /\s/ && $depth == 0;
                $depth++ if $ch eq '(';
                $depth-- if $ch eq ')' && $depth > 0;
            }
            $target .= $ch;
        }
    }

    $target =~ s/^\s+|\s+$//g;
    $target =~ s/\\([\\`*_\{\}\[\]\(\)#\+\-.!<> ])/$1/g;
    return $target;
}

sub extract_inline_link_targets {
    my ($line) = @_;
    my @targets;
    my $len = length($line);
    my $pos = 0;

    while (($pos = index($line, '[', $pos)) != -1) {
        if (is_escaped($line, $pos)) {
            $pos++;
            next;
        }

        my $close_bracket = find_closing_bracket($line, $pos + 1);
        last if $close_bracket < 0;

        my $open_paren = $close_bracket + 1;
        if ($open_paren >= $len || substr($line, $open_paren, 1) ne '(') {
            $pos = $close_bracket + 1;
            next;
        }

        my ($raw_target, $close_paren) = parse_parenthesized_link($line, $open_paren + 1);
        if (defined $raw_target) {
            my $target = normalize_link_target($raw_target);
            push @targets, $target if $target ne '';
            $pos = $close_paren + 1;
        } else {
            $pos = $close_bracket + 1;
        }
    }

    return @targets;
}
PERL
}

files_checked=0
links_checked=0
broken_links=0

while IFS= read -r -d '' md_file; do
    if [ ! -f "$md_file" ]; then
        if [ "${#FILES[@]}" -gt 0 ]; then
            echo "WARN: skipping missing file argument: $md_file" >&2
        fi
        continue
    fi

    files_checked=$((files_checked + 1))
    if [ "$QUIET" = "false" ]; then
        echo "Checking links in: $md_file"
    fi

    base_dir=$(dirname -- "$md_file")

    while IFS=$'\t' read -r line_num url; do
        [ -n "$url" ] || continue

        case "$url" in
            http://*|https://*|mailto:*|tel:*|\#*)
                continue
                ;;
        esac

        file_part="${url%%#*}"
        [ -n "$file_part" ] || continue

        links_checked=$((links_checked + 1))

        case "$file_part" in
            /*)
                echo "$md_file:$line_num: absolute link '$url' is not portable; use a repository-relative Markdown link"
                broken_links=$((broken_links + 1))
                continue
                ;;
        esac

        full_path=$(normalize_path "$base_dir/$file_part")

        if [ ! -e "$full_path" ]; then
            echo "$md_file:$line_num: link '$url' -> file not found (resolved to $full_path)"
            broken_links=$((broken_links + 1))
            continue
        fi

        if [ "$CHECK_TRACKED" = "true" ]; then
            if relative_target=$(repo_relative_path "$full_path"); then
                if ! target_is_tracked "$full_path" "$relative_target"; then
                    echo "$md_file:$line_num: link '$url' -> target exists locally but is not tracked by git (resolved to $full_path)"
                    echo "  CI checks out tracked files only; commit the target or link to a tracked file."
                    broken_links=$((broken_links + 1))
                fi
            else
                echo "$md_file:$line_num: link '$url' -> target resolves outside the repository (resolved to $full_path)"
                echo "  Internal documentation links must stay within the repository so they work in CI and code review."
                broken_links=$((broken_links + 1))
            fi
        fi
    done < <(extract_markdown_links "$md_file")
done < <(list_markdown_files)

if [ "$broken_links" -gt 0 ]; then
    echo ""
    echo "Found $broken_links broken internal link(s) across $files_checked Markdown file(s)."
    exit 1
fi

if [ "$QUIET" = "false" ]; then
    echo "All $links_checked internal link(s) in $files_checked Markdown file(s) are valid."
fi
