#!/usr/bin/env bash
# check-markdown-link-text.sh
#
# Enforces human-readable link text for internal markdown links.
#
# Policy:
#   Avoid filename-as-label links such as:
#     [testing-core-patterns](./testing-core-patterns.md)
#   Prefer descriptive labels such as:
#     [Core Testing Patterns](./testing-core-patterns.md)
#
# Usage:
#   ./scripts/check-markdown-link-text.sh
#   ./scripts/check-markdown-link-text.sh --files path/a.md path/b.md
#   ./scripts/check-markdown-link-text.sh --fix
#   ./scripts/check-markdown-link-text.sh --fix --files path/a.md

set -euo pipefail

FIX_MODE=0
FILE_ARGS_MODE=0
REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || echo ".")
cd "$REPO_ROOT"

if [ -t 1 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    BLUE='\033[0;34m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    NC=''
fi

usage() {
    cat <<'EOF'
Usage:
  ./scripts/check-markdown-link-text.sh
  ./scripts/check-markdown-link-text.sh --files path/a.md path/b.md
  ./scripts/check-markdown-link-text.sh --fix
  ./scripts/check-markdown-link-text.sh --fix --files path/a.md
EOF
}

declare -a FILES_TO_CHECK=()

while [ "$#" -gt 0 ]; do
    case "$1" in
        --fix)
            FIX_MODE=1
            shift
            ;;
        --files)
            FILE_ARGS_MODE=1
            shift
            if [ "$#" -eq 0 ]; then
                echo -e "${RED}[ERROR]${NC} --files requires at least one file argument" >&2
                exit 2
            fi
            while [ "$#" -gt 0 ]; do
                case "$1" in
                    --fix|--files)
                        break
                        ;;
                    *)
                        FILES_TO_CHECK+=("$1")
                        shift
                        ;;
                esac
            done
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo -e "${RED}[ERROR]${NC} Unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [ "$FILE_ARGS_MODE" -eq 0 ]; then
    while IFS= read -r path; do
        FILES_TO_CHECK+=("$path")
    done < <(git ls-files '*.md')
fi

echo -e "${BLUE}Markdown Link Text Checker${NC}"
echo "Repository: $REPO_ROOT"
if [ "$FIX_MODE" -eq 1 ]; then
    echo "Mode: fix"
else
    echo "Mode: check"
fi
echo ""

if [ "$FILE_ARGS_MODE" -eq 1 ]; then
    echo -e "${BLUE}[INFO]${NC} Scanning ${#FILES_TO_CHECK[@]} explicitly provided file(s)..."
else
    echo -e "${BLUE}[INFO]${NC} Scanning tracked markdown files..."
fi

echo ""

TMP_REPORT=$(mktemp)
trap 'rm -f "$TMP_REPORT"' EXIT

CHECKED=0
MISSING=0

if [ "${#FILES_TO_CHECK[@]}" -gt 0 ]; then
    for file in "${FILES_TO_CHECK[@]}"; do
        if [ ! -f "$file" ]; then
            echo -e "${YELLOW}[WARN]${NC} Skipping non-existent file: $file"
            MISSING=$((MISSING + 1))
            continue
        fi

    case "$file" in
        *.md) ;;
        *) continue ;;
    esac

    CHECKED=$((CHECKED + 1))

    FIX_FLAG="$FIX_MODE" perl - "$file" "$TMP_REPORT" <<'PERL'
use strict;
use warnings;

my ($path, $report_path) = @ARGV;
my $fix_mode = $ENV{FIX_FLAG} // 0;

open my $in, '<', $path or die "failed to read $path: $!";
my @lines = <$in>;
close $in;

my %acronyms = (
    'api' => 'API',
    'ci' => 'CI',
    'cd' => 'CD',
    'msrv' => 'MSRV',
    'llm' => 'LLM',
    'aws' => 'AWS',
    'cpu' => 'CPU',
    'gpu' => 'GPU',
    'http' => 'HTTP',
    'https' => 'HTTPS',
    'json' => 'JSON',
    'yaml' => 'YAML',
    'toml' => 'TOML',
    'rust' => 'Rust',
    'github' => 'GitHub',
    'websocket' => 'WebSocket',
    'ddos' => 'DDoS',
    'adr' => 'ADR',
);

sub normalize {
    my ($s) = @_;
    $s = lc($s // '');
    $s =~ s/\.md$//;
    $s =~ s/[^a-z0-9]+//g;
    return $s;
}

sub humanize {
    my ($stem) = @_;
    my @parts = grep { length($_) > 0 } split /[-_]+/, lc($stem // '');
    my @out;
    for my $p (@parts) {
        if (exists $acronyms{$p}) {
            push @out, $acronyms{$p};
        } else {
            push @out, ucfirst($p);
        }
    }
    return join(' ', @out);
}

my $in_fence = 0;
my $violations = 0;

for (my $i = 0; $i < scalar(@lines); $i++) {
    my $line = $lines[$i];

    if ($line =~ /^```/) {
        $in_fence = !$in_fence;
        next;
    }

    next if $in_fence;

    $line =~ s{\[([^\]]+)\]\((\.?\.?\/[^)\s]+?\.md(?:#[^)\s]+)?)\)}{
        my ($text, $target) = ($1, $2);
        my $target_no_anchor = $target;
        $target_no_anchor =~ s/#.*$//;
        my $base = $target_no_anchor;
        $base =~ s{^.*/}{};
        my $stem = $base;
        $stem =~ s/\.md$//;
        if (lc($base) eq 'skill.md') {
            my $skill_dir = $target_no_anchor;
            $skill_dir =~ s{/SKILL\.md$}{}i;
            $skill_dir =~ s{^.*/}{};
            $stem = $skill_dir;
        }

        my $normalized_text = normalize($text);
        my $normalized_stem = normalize($stem);
        my $looks_filename_style = (
            $text =~ /\.md$/i ||
            $text eq $stem ||
            $text eq $base
        );

        if ($looks_filename_style && $normalized_text eq $normalized_stem) {
            my $new_text = humanize($stem);
            $violations++;
            open my $rep, '>>', $report_path or die "failed to append report: $!";
            print {$rep} "$path:" . ($i + 1) . ": [$text]($target) -> [$new_text]($target)\n";
            close $rep;
            if ($fix_mode) {
                "[$new_text]($target)";
            } else {
                "[$text]($target)";
            }
        } else {
            "[$text]($target)";
        }
    }ge;

    $lines[$i] = $line;
}

if ($fix_mode && $violations > 0) {
    open my $out, '>', $path or die "failed to write $path: $!";
    print {$out} @lines;
    close $out;
}
PERL
    done
fi

VIOLATION_COUNT=$(wc -l < "$TMP_REPORT" | tr -d '[:space:]')

if [ "$VIOLATION_COUNT" -gt 0 ]; then
    echo -e "${RED}[ERROR]${NC} Found $VIOLATION_COUNT filename-style internal markdown link(s)."
    echo ""
    sed -n '1,200p' "$TMP_REPORT"
    echo ""
    if [ "$FIX_MODE" -eq 1 ]; then
        echo -e "${GREEN}[OK]${NC} Applied auto-fixes for all reported links."
        exit 0
    fi
    echo "Run './scripts/check-markdown-link-text.sh --fix' to auto-fix."
    exit 1
fi

echo -e "${GREEN}[OK]${NC} No filename-style internal markdown links found."
if [ "$MISSING" -gt 0 ]; then
    echo -e "${YELLOW}[WARN]${NC} Skipped $MISSING missing file argument(s)."
fi
exit 0
