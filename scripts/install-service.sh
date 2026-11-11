#!/usr/bin/env bash
# Install / uninstall the McloudClient kernel as a Windows service.
#
# Usage (run from Git Bash as Administrator):
#   PROFILE=/d/wxmuma/_c0d_test/profile.json ./scripts/install-service.sh install
#   ./scripts/install-service.sh status
#   ./scripts/install-service.sh uninstall
#
# Environment:
#   KERNEL        path to client-core.exe  (default: target/debug/client-core.exe)
#   PROFILE       optional seed profile; only copied to live.json when absent
#   SNAPSHOT_DIR  snapshot directory       (default: %ProgramData%\McloudClient\snapshots)
#   PIPE          named pipe               (default: \\.\pipe\mcloud-client)
#   LOG_DIR       log directory            (default: %ProgramData%\McloudClient\logs)
#   SERVICE       service name             (default: McloudCore)
#
# The kernel reads <SNAPSHOT_DIR>\live.json, never the raw subscription file,
# so `client-sidecar --snapshot-dir <SNAPSHOT_DIR>` must point at the same place.

set -euo pipefail

SERVICE="${SERVICE:-McloudCore}"
KERNEL="${KERNEL:-target/debug/client-core.exe}"
# Single quotes keep backslashes literal; double quotes would collapse \\ to \.
if [[ -z "${PIPE:-}" ]]; then
    PIPE='\\.\pipe\mcloud-client'
fi
if [[ -z "${LOG_DIR:-}" ]]; then
    LOG_DIR='C:\ProgramData\McloudClient\logs'
fi
# The kernel must read the snapshot the sidecar writes (ADR 0002 section 2),
# never the raw subscription file.
if [[ -z "${SNAPSHOT_DIR:-}" ]]; then
    SNAPSHOT_DIR='C:\ProgramData\McloudClient\snapshots'
fi

usage() {
    sed -n '2,20p' "$0"
    exit 2
}

require_admin() {
    if ! net.exe session >/dev/null 2>&1; then
        echo "error: administrator privileges required" >&2
        exit 1
    fi
}

# SCM starts services with C:\Windows\System32 as the working directory,
# so every path in binPath must be absolute.
abs_path() {
    local dir base
    dir="$(cd "$(dirname "$1")" && pwd)"
    base="$(basename "$1")"
    echo "${dir}/${base}"
}

install() {
    require_admin

    local kernel snapshot_unix snapshot_win
    kernel="$(cygpath -w "$(abs_path "$KERNEL")")"
    snapshot_unix="$(cygpath -u "$SNAPSHOT_DIR")"
    mkdir -p "$snapshot_unix"

    # Bootstrap: the kernel must never start with an empty/missing config.
    if [[ ! -f "$snapshot_unix/live.json" ]]; then
        if [[ -n "${PROFILE:-}" ]]; then
            cp "$(cygpath -u "$(abs_path "$PROFILE")")" "$snapshot_unix/live.json"
        else
            printf '%s' '{"proxies":[],"proxy-groups":[],"rules":["MATCH,DIRECT"]}' \
                > "$snapshot_unix/live.json"
        fi
    fi
    snapshot_win="$(cygpath -w "$snapshot_unix")"

    local bin_path
    bin_path="\"${kernel}\" --service --profile \"${snapshot_win}\\live.json\" --ipc-pipe ${PIPE} --log-dir \"${LOG_DIR}\""

    sc.exe create "$SERVICE" binPath= "$bin_path" start= auto DisplayName= "McloudClient Core"
    sc.exe description "$SERVICE" "McloudClient data plane (VLESS/WS/TLS/mlkem)"
    # Restart on crash: 5 s, 10 s, then 30 s; reset the counter after 24 h.
    sc.exe failure "$SERVICE" reset= 86400 actions= restart/5000/restart/10000/restart/30000
    sc.exe start "$SERVICE"
}

uninstall() {
    require_admin
    sc.exe stop "$SERVICE" >/dev/null 2>&1 || true
    sleep 2
    sc.exe delete "$SERVICE"
}

case "${1:-}" in
    install) install ;;
    uninstall) uninstall ;;
    status) sc.exe query "$SERVICE" ;;
    *) usage ;;
esac
