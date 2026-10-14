#!/usr/bin/env bash
set -euo pipefail

printf 'available_processors=%s\n' "$(nproc)"
awk '/^Cpus_allowed_list:/ { print }' /proc/self/status

record_cpu_file() {
    local cpu_evidence_file="$1"
    printf '%s:\n' "$cpu_evidence_file"
    if [[ -r "$cpu_evidence_file" ]]; then
        cat "$cpu_evidence_file"
    else
        printf 'unavailable\n'
    fi
}

for cpu_evidence_file in \
    /proc/self/cgroup \
    /sys/fs/cgroup/cpu/cpu.cfs_quota_us \
    /sys/fs/cgroup/cpu/cpu.cfs_period_us \
    /proc/loadavg \
    /proc/pressure/cpu; do
    record_cpu_file "$cpu_evidence_file"
done

cpu_cgroup_relative_path=$(awk -F: '$1 == "0" { print $3 }' /proc/self/cgroup)
cpu_cgroup_directory=$(realpath -m "/sys/fs/cgroup${cpu_cgroup_relative_path}")
case "$cpu_cgroup_directory" in
    /sys/fs/cgroup | /sys/fs/cgroup/*) ;;
    *)
        printf 'The process cgroup path is outside the mounted namespace.\n'
        cpu_cgroup_directory=/sys/fs/cgroup
        ;;
esac

while true; do
    record_cpu_file "$cpu_cgroup_directory/cpu.max"
    record_cpu_file "$cpu_cgroup_directory/cpu.stat"
    if [[ "$cpu_cgroup_directory" == /sys/fs/cgroup ]]; then
        break
    fi
    cpu_cgroup_directory=$(dirname "$cpu_cgroup_directory")
done
