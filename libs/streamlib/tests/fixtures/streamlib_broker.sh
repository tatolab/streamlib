#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# E2E fixture: start / stop / probe a streamlib-broker daemon using a
# pre-built binary from target/debug. Intended for integration tests that
# need a real broker running (not the in-process UnixSocketSurfaceService)
# but should not pay the cost of `cargo run` inside the test.
#
# Usage:
#   streamlib_broker.sh start [socket_path]
#       Build the broker if needed, spawn it, probe until ready.
#       Prints the socket path on stdout; the pid is written to
#       $STREAMLIB_BROKER_FIXTURE_PID (default: same dir as socket).
#   streamlib_broker.sh stop [socket_path]
#       Stop the daemon, remove the pid file + socket file.
#   streamlib_broker.sh probe [socket_path]
#       Connect + ping; exit 0 on pong, 1 otherwise.
#
# Default socket path: $(mktemp -d)/broker.sock per start invocation.
#
# The fixture always uses target/debug — it is a test harness, not a dev
# daemon. For ad-hoc dev use, prefer scripts/streamlib-broker-dev.sh.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
BROKER_BIN="${REPO_ROOT}/target/debug/streamlib-broker"

build_if_missing() {
    if [[ ! -x "$BROKER_BIN" ]]; then
        echo "[streamlib_broker.sh] building streamlib-broker..." >&2
        cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" -p streamlib-broker --quiet
    fi
}

probe_cmd() {
    local socket="$1"
    "$BROKER_BIN" --probe "$socket"
}

start() {
    build_if_missing
    local socket="${1:-}"
    if [[ -z "$socket" ]]; then
        local dir
        dir="$(mktemp -d -t streamlib-broker-fixture.XXXXXX)"
        socket="$dir/broker.sock"
    else
        mkdir -p "$(dirname "$socket")"
    fi
    local pid_file="${STREAMLIB_BROKER_FIXTURE_PID:-${socket%.sock}.pid}"
    local log_file="${socket%.sock}.log"

    # Use a port that is unlikely to collide with a running dev broker (50052)
    # or production (50051). Let the OS pick via 0 would be nicer, but the
    # broker's CLI takes an explicit u16. Use 50099 as the fixture default,
    # allow STREAMLIB_BROKER_FIXTURE_PORT to override for parallel runs.
    local port="${STREAMLIB_BROKER_FIXTURE_PORT:-50099}"

    setsid "$BROKER_BIN" \
        --port "$port" \
        --socket-path "$socket" \
        >"$log_file" 2>&1 &
    local pid=$!
    echo "$pid" > "$pid_file"

    # Wait for readiness via --probe.
    local attempt=0
    while (( attempt < 50 )); do
        if probe_cmd "$socket" >/dev/null 2>&1; then
            echo "$socket"
            return 0
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "[streamlib_broker.sh] broker exited before ready; log tail:" >&2
            tail -20 "$log_file" >&2 || true
            rm -f "$pid_file"
            return 1
        fi
        sleep 0.1
        attempt=$((attempt + 1))
    done

    echo "[streamlib_broker.sh] broker did not become ready within 5s; log tail:" >&2
    tail -20 "$log_file" >&2 || true
    kill "$pid" 2>/dev/null || true
    rm -f "$pid_file"
    return 1
}

stop() {
    local socket="${1:-}"
    if [[ -z "$socket" ]]; then
        echo "usage: $0 stop <socket_path>" >&2
        return 2
    fi
    local pid_file="${STREAMLIB_BROKER_FIXTURE_PID:-${socket%.sock}.pid}"
    if [[ -f "$pid_file" ]]; then
        local pid
        pid="$(cat "$pid_file")"
        if kill -0 "$pid" 2>/dev/null; then
            kill "$pid" 2>/dev/null || true
            # Wait up to 2s for clean shutdown.
            local attempt=0
            while kill -0 "$pid" 2>/dev/null; do
                if (( attempt >= 20 )); then
                    kill -9 "$pid" 2>/dev/null || true
                    break
                fi
                sleep 0.1
                attempt=$((attempt + 1))
            done
        fi
        rm -f "$pid_file"
    fi
    [[ -S "$socket" ]] && rm -f "$socket"
}

probe() {
    local socket="${1:-}"
    if [[ -z "$socket" ]]; then
        echo "usage: $0 probe <socket_path>" >&2
        return 2
    fi
    probe_cmd "$socket"
}

cmd="${1:-}"
shift || true
case "$cmd" in
    start) start "$@" ;;
    stop)  stop "$@" ;;
    probe) probe "$@" ;;
    *)
        echo "usage: $0 {start [socket]|stop <socket>|probe <socket>}" >&2
        exit 2
        ;;
esac
