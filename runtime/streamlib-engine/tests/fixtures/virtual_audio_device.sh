#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Virtual audio device fixture for E2E tests. Sister to virtual_camera.sh:
# where that one gives the rig a camera with no camera, this one gives it an
# audio device with no sound card, no jack, and nobody logged in.
#
# Usage:
#   ./virtual_audio_device.sh check   — is a session reachable and are the tools here
#   ./virtual_audio_device.sh start   — create the null sink, print its name
#   ./virtual_audio_device.sh stop    — destroy it
#
# Requires: a running PipeWire session (pw-cli, pw-play, pw-record).
#
# A null sink is used rather than a virtual source because its monitor is a
# capture endpoint the session already routes: whatever is played into the sink
# is readable from the monitor, which is the loopback the fixture needs.
set -uo pipefail

NODE_NAME="streamlib-fixture-audio-sink"

node_id_of_the_fixture_sink() {
    pw-cli ls Node 2>/dev/null \
        | awk -v name="\"$NODE_NAME\"" '
            /^[[:space:]]*id / { id = $2; sub(/,$/, "", id) }
            $0 ~ "node.name = " name { print id; exit }
        '
}

case "${1:-}" in
    check)
        for tool in pw-cli pw-play pw-record; do
            if ! command -v "$tool" &>/dev/null; then
                echo "UNAVAILABLE: $tool not found"
                exit 1
            fi
        done
        if ! timeout 10 pw-cli info 0 &>/dev/null; then
            echo "UNAVAILABLE: no PipeWire session is reachable"
            exit 1
        fi
        echo "AVAILABLE: PipeWire session reachable"
        exit 0
        ;;
    start)
        if [ -n "$(node_id_of_the_fixture_sink)" ]; then
            echo "$NODE_NAME"
            exit 0
        fi
        # `object.linger` keeps the node alive after the creating pw-cli exits;
        # without it the sink dies with the command that made it.
        timeout 10 pw-cli create-node adapter "{ factory.name=support.null-audio-sink \
            node.name=$NODE_NAME node.description=\"StreamLib Fixture Audio Sink\" \
            media.class=Audio/Sink object.linger=true audio.position=[FL FR] }" >/dev/null 2>&1
        for _ in $(seq 20); do
            if [ -n "$(node_id_of_the_fixture_sink)" ]; then
                echo "$NODE_NAME"
                exit 0
            fi
            sleep 0.25
        done
        # The node was created before this wait began, so giving up without
        # destroying it would leave one behind in the user's live session —
        # and `object.linger` means it outlives every process here.
        "$0" stop >/dev/null 2>&1
        echo "ERROR: the null sink did not appear in the graph" >&2
        exit 1
        ;;
    stop)
        id="$(node_id_of_the_fixture_sink)"
        if [ -z "$id" ]; then
            echo "not running"
            exit 0
        fi
        timeout 10 pw-cli destroy "$id" >/dev/null 2>&1
        # Confirmed rather than assumed: a sink left behind can be promoted to
        # the session default and silence the machine, so a cleanup that failed
        # has to say so rather than report success.
        if [ -n "$(node_id_of_the_fixture_sink)" ]; then
            echo "ERROR: $NODE_NAME survived destroy and is still in the graph" >&2
            exit 1
        fi
        echo "stopped"
        exit 0
        ;;
    *)
        echo "Usage: $0 {check|start|stop}" >&2
        exit 1
        ;;
esac
