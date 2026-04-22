#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# StreamLib Broker - dev-mode lifecycle helper.
#
# Starts / stops / probes a `cargo run -p streamlib-broker` daemon backed by
# the current source tree. The invariant this enforces: in dev, the broker
# you connect to is whatever the working tree currently builds.
#
# Usage:
#   streamlib-broker-dev.sh start     # start if not already running
#   streamlib-broker-dev.sh stop      # stop the running dev broker
#   streamlib-broker-dev.sh restart   # stop then start
#   streamlib-broker-dev.sh status    # print status + PID
#   streamlib-broker-dev.sh probe     # exit 0 if the socket responds, 1 otherwise
#
# Env:
#   STREAMLIB_HOME           default: $REPO_ROOT/.streamlib
#   STREAMLIB_BROKER_SOCKET  default: $STREAMLIB_HOME/broker.sock
#   STREAMLIB_BROKER_PORT    default: 50052
#   STREAMLIB_BROKER_LOG     default: /tmp/streamlib-broker-dev.log
#
# The pidfile / lockfile at $STREAMLIB_HOME/broker.pid prevents double-spawn.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

STREAMLIB_HOME="${STREAMLIB_HOME:-${REPO_ROOT}/.streamlib}"
BROKER_SOCKET="${STREAMLIB_BROKER_SOCKET:-${STREAMLIB_HOME}/broker.sock}"
BROKER_PORT="${STREAMLIB_BROKER_PORT:-50052}"
BROKER_LOG="${STREAMLIB_BROKER_LOG:-/tmp/streamlib-broker-dev.log}"
PID_FILE="${STREAMLIB_HOME}/broker.pid"

log() { echo "[streamlib-broker-dev] $*" >&2; }

ensure_home() {
    mkdir -p "$STREAMLIB_HOME"
}

is_running() {
    # Returns 0 if a PID file exists and the process is alive.
    [[ -f "$PID_FILE" ]] || return 1
    local pid
    pid="$(cat "$PID_FILE" 2>/dev/null || true)"
    [[ -n "$pid" ]] || return 1
    kill -0 "$pid" 2>/dev/null
}

probe() {
    # Uses the broker binary's own --probe subcommand (connects + pings).
    cargo run --manifest-path "${REPO_ROOT}/Cargo.toml" -p streamlib-broker --quiet -- \
        --probe "$BROKER_SOCKET"
}

start() {
    ensure_home

    if is_running; then
        log "already running (pid $(cat "$PID_FILE"))"
        return 0
    fi

    # Build once so the subsequent cargo-run is fast and the "start" completion
    # signal reflects a ready binary, not a compile-in-progress.
    log "building streamlib-broker (debug)..."
    cargo build --manifest-path "${REPO_ROOT}/Cargo.toml" -p streamlib-broker --quiet

    log "starting broker (socket $BROKER_SOCKET, port $BROKER_PORT)"
    # Use `setsid` so the broker keeps running when this shell exits. Redirect
    # stdout/stderr to the log file. Record the child pid.
    STREAMLIB_HOME="$STREAMLIB_HOME" \
    STREAMLIB_BROKER_SOCKET="$BROKER_SOCKET" \
    setsid "${REPO_ROOT}/target/debug/streamlib-broker" \
        --port "$BROKER_PORT" \
        --socket-path "$BROKER_SOCKET" \
        >"$BROKER_LOG" 2>&1 &
    echo "$!" > "$PID_FILE"

    # Poll the socket via --probe until it responds (up to 10s).
    local attempt=0
    while (( attempt < 50 )); do
        if probe >/dev/null 2>&1; then
            log "broker ready (pid $(cat "$PID_FILE"), log $BROKER_LOG)"
            return 0
        fi
        sleep 0.2
        attempt=$((attempt + 1))
    done

    log "broker did not become ready within 10s; tail -20 of log:"
    tail -20 "$BROKER_LOG" >&2 || true
    return 1
}

stop() {
    if ! is_running; then
        log "not running"
        rm -f "$PID_FILE"
        return 0
    fi
    local pid
    pid="$(cat "$PID_FILE")"
    log "stopping broker (pid $pid)"
    kill "$pid" 2>/dev/null || true
    # Wait up to 5s for clean shutdown.
    local attempt=0
    while kill -0 "$pid" 2>/dev/null; do
        if (( attempt >= 25 )); then
            log "broker did not exit cleanly, SIGKILL"
            kill -9 "$pid" 2>/dev/null || true
            break
        fi
        sleep 0.2
        attempt=$((attempt + 1))
    done
    rm -f "$PID_FILE"
    # Socket file should be cleaned up by broker's own Drop, but be safe.
    [[ -S "$BROKER_SOCKET" ]] && rm -f "$BROKER_SOCKET"
    log "stopped"
}

status() {
    if is_running; then
        local pid
        pid="$(cat "$PID_FILE")"
        echo "running (pid $pid, socket $BROKER_SOCKET)"
        return 0
    fi
    echo "not running"
    return 1
}

cmd="${1:-status}"
case "$cmd" in
    start)   start ;;
    stop)    stop ;;
    restart) stop; start ;;
    status)  status ;;
    probe)   probe ;;
    *)
        echo "usage: $0 {start|stop|restart|status|probe}" >&2
        exit 2
        ;;
esac
