# Identify and rewrite the unsourced signal-fish-server package block in Cargo.lock.
#
# Modes:
#   list    - print each input filename followed by NUL when it has an unsourced entry
#   state   - print <unsourced entries>:<entries matching expected_version>
#   rewrite - copy the lockfile to stdout, replacing only the unsourced entry version
# `state` and `rewrite` have a deliberate single-input contract because their
# output formats do not carry filenames; fail closed instead of pooling counters.

BEGIN {
    input_files = 0
    for (argument_index = 1; argument_index < ARGC; argument_index++) {
        if (ARGV[argument_index] == "--") {
            # Awk implementations may retain the option sentinel as a filename.
            ARGV[argument_index] = ""
        } else if (ARGV[argument_index] != "") {
            input_files++
        }
    }
    input_error = mode != "list" && input_files != 1
    if (input_error) {
        print "ERROR: " mode " mode accepts exactly one input file." > "/dev/stderr"
        exit 2
    }

    current_file = ""
    entries = 0
    matching = 0
    changed = 0
    reset_package()
}

function reset_package() {
    in_package = 0
    package_buffer = ""
    package_name = ""
    package_version = ""
    package_has_source = 0
}

function flush_package(    is_target, lines, line_count, line_index, line, replaced) {
    if (!in_package) return

    is_target = package_name == "signal-fish-server" && !package_has_source
    if (is_target) {
        entries++
        if (mode == "state" && package_version == expected_version) matching++
    }

    if (mode == "rewrite") {
        line_count = split(package_buffer, lines, "\n")
        replaced = 0
        for (line_index = 1; line_index < line_count; line_index++) {
            line = lines[line_index]
            if (is_target && !replaced && line ~ /^[[:space:]]*version[[:space:]]*=[[:space:]]*"/) {
                sub(/"[^"]*"/, "\"" next_version "\"", line)
                print line
                replaced = 1
                changed++
            } else {
                print line
            }
        }
    }

    reset_package()
}

function finish_file() {
    flush_package()
    if (mode == "list") {
        if (entries > 0) printf "%s%c", current_file, 0
        entries = 0
        matching = 0
    }
}

FNR == 1 {
    if (current_file != "") finish_file()
    current_file = FILENAME
}

/^[[:space:]]*\[\[package\]\][[:space:]]*(#.*)?$/ {
    flush_package()
    in_package = 1
    package_buffer = $0 ORS
    next
}

in_package {
    package_buffer = package_buffer $0 ORS
    if ($0 ~ /^[[:space:]]*name[[:space:]]*=[[:space:]]*"signal-fish-server"[[:space:]]*(#.*)?$/) {
        package_name = "signal-fish-server"
    }
    if ($0 ~ /^[[:space:]]*version[[:space:]]*=[[:space:]]*"/ && package_version == "") {
        package_version = $0
        sub(/^[^"]*"/, "", package_version)
        sub(/".*$/, "", package_version)
    }
    if ($0 ~ /^[[:space:]]*source[[:space:]]*=[[:space:]]*"/) package_has_source = 1
    next
}

mode == "rewrite" { print }

END {
    if (input_error) exit 2
    if (current_file != "") finish_file()
    if (mode == "state") print entries ":" matching
    if (mode == "rewrite") {
        print changed > count_file
        close(count_file)
    }
}
