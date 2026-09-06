#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The delivery deadline's measured before/after, across a shaped uplink.
#
# Two arms of the same graph through the same relay, one with the policy off
# and one with it on. The baseline is as much of the deliverable as the
# improvement is, so both numbers are printed even when they are the same.
#
# THE SOURCE HAS TO CARRY BITS. vivid's default pattern is a flat colour and
# encodes to roughly 365 bytes per delta frame — about 15 kbit/s, which no
# ceiling this script could set would ever congest; every other static or
# moving pattern inter-predicts to under 2 KB a frame. Pattern 21 is Noise,
# which at constant QP is 2.3 MB a frame, about 93 Mbit/s. There is nothing in
# between: `H264Encoder`'s `bitrate_bps` fails its first encode on the rig with
# `ERROR_INITIALIZATION`, so the ceiling is set here rather than at the source,
# and SHAPED_RATE is read against 93 Mbit/s. All measured on the rig,
# 2026-09-05.
#
# VIDEO ONLY. The CMAF init segment waits on every declared track, and a
# microphone with nothing to say holds the broadcast until the hold's byte
# bound stops it — which is a measurement of the hold, not of the policy.
#
# WHAT THIS MEASURES, and what it does not. It reports what the publisher shed
# and abandoned, what the uplink was behind by, and what reached the decoder.
# The deadline reads two things: a bag's age at the publisher's input, and the
# uplink backlog — the vendored `moq-transport`'s forwarder cursor, which says
# how many of a group's objects have not left the QUIC send window. A policy
# arm whose teardown line counts sheds "begun on the backlog" and "groups
# abandoned" is the link falling behind, seen from the publisher; the QUIC
# path's rtt, cwnd and loss counters ride the same line. The baseline arm's
# decoder counts under a ceiling are how far a reliable transport falls
# behind, and its "undelivered at teardown" is the backlog the policy arm has
# instead of.
# Glass-to-glass latency is NOT measured here: under `cmaf` a received bag's
# stamp is the fragment's decode time on the subscriber's own clock rather than
# the producer's, so the two ends share no epoch to subtract. Measuring it
# wants the `streamlib_bag` container and a tap, and is its own fixture.
#
# ROOT. `tc` shapes the default route's egress, which is every other thing this
# machine sends for as long as an arm runs. Arms are short and the qdisc is
# removed on any exit, including a signal.
#
# Usage:
#   sudo -v && packages/streamlib-moq/tests/live/moq_drop_policy_shaped_link.sh [output_dir]
#
# Environment:
#   STREAMLIB_MOQ_RELAY_URL   the relay, token in the path. Falls back to
#                             CLOUDFLARE_MOQ_DRAFT_16_URL + the publish token
#                             from the repo-root `.env`, as the round-trip
#                             fixture does.
#   DELIVERY_DEADLINE_MS      the policy arm's deadline (default 200)
#   SHAPED_RATE               the uplink ceiling (default 20mbit — a fifth of
#                             what Noise offers; 400kbit decoded nothing)
#   SHAPED_DELAY              one-way delay netem adds (default 80ms)
#   SHAPED_LOSS               loss netem adds (default 1%)
#   ARM_SECONDS               how long each arm runs (default 60)
#   VIVID_TEST_PATTERN        vivid pattern index (default 21 = Noise)
#   CONTROL_PLANE_PORT        default 9415
#
# Exit codes: 0 = both arms ran, 1 = an arm failed, 77 = cannot run.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PACKAGE_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
REPO_ROOT="$(cd "$PACKAGE_DIR/../.." && pwd)"

OUTPUT_DIR="${1:-/tmp/streamlib-moq-shaped-link-$(date +%s)}"
DELIVERY_DEADLINE_MS="${DELIVERY_DEADLINE_MS:-200}"
SHAPED_RATE="${SHAPED_RATE:-20mbit}"
SHAPED_DELAY="${SHAPED_DELAY:-80ms}"
SHAPED_LOSS="${SHAPED_LOSS:-1%}"
ARM_SECONDS="${ARM_SECONDS:-60}"
VIVID_TEST_PATTERN="${VIVID_TEST_PATTERN:-21}"
CONTROL_PLANE_PORT="${CONTROL_PLANE_PORT:-9415}"

say() { echo "[moq-shaped] $*"; }
cannot_run() { echo "[moq-shaped] SKIP: $*" >&2; exit 77; }
fail() { echo "[moq-shaped] FAIL: $*" >&2; exit 1; }

for tool in ip tc v4l2-ctl timeout; do
    command -v "$tool" >/dev/null || cannot_run "missing: $tool"
done
compgen -G "/dev/dri/renderD*" >/dev/null \
    || cannot_run "no DRM render node, so no GPU-backed Runtime can start here"

VENV_PYTHON="$PACKAGE_DIR/.venv/bin/python"
[ -x "$VENV_PYTHON" ] || cannot_run \
    "no venv at $PACKAGE_DIR/.venv — create it and \`maturin develop\` this wheel into it"

# This arm scores whatever `_native.so` that venv holds, so a stale extension
# would be measured and reported for code that is not in the tree.
"$VENV_PYTHON" -c '
import streamlib
from streamlib_moq import MoqBroadcastPublisher, MoqBroadcastSubscriber
_ = (streamlib.H264Decoder, MoqBroadcastPublisher, MoqBroadcastSubscriber)
' >/dev/null 2>&1 || cannot_run "the venv cannot import the engine and this wheel"

# `sudo -n` rather than a prompt: this is a fixture, and a script that blocks on
# a password reads from outside as a hang.
sudo -n true 2>/dev/null || cannot_run \
    "tc needs root and sudo wants a password. Run \`sudo -v\` first, then this."

# ── Credentials ──────────────────────────────────────────────────────
# The relay URL is itself a credential and never reaches a log, an error, or
# this script's output directory.
if [ -z "${STREAMLIB_MOQ_RELAY_URL:-}" ] && [ -f "$REPO_ROOT/.env" ]; then
    set -a
    # shellcheck disable=SC1091
    . "$REPO_ROOT/.env" >/dev/null 2>&1 || true
    set +a
fi
if [ -z "${STREAMLIB_MOQ_RELAY_URL:-}" ] \
    && [ -n "${CLOUDFLARE_MOQ_DRAFT_16_URL:-}" ] \
    && [ -n "${CLOUDFLARE_MOQ_PUB_SUB_TOKEN:-}" ]; then
    STREAMLIB_MOQ_RELAY_URL="https://${CLOUDFLARE_MOQ_DRAFT_16_URL#https://}/${CLOUDFLARE_MOQ_PUB_SUB_TOKEN}"
fi
[ -n "${STREAMLIB_MOQ_RELAY_URL:-}" ] || cannot_run \
    "no relay credential. Export STREAMLIB_MOQ_RELAY_URL, or put CLOUDFLARE_MOQ_DRAFT_16_URL and CLOUDFLARE_MOQ_PUB_SUB_TOKEN in the repo-root .env. Absent credentials are a cannot-run, never a pass."
export STREAMLIB_MOQ_RELAY_URL

# ── The camera ───────────────────────────────────────────────────────
# `/proc/modules` rather than `lsmod | grep -q`: under `pipefail`, `grep -q`
# closing the pipe on its first match kills `lsmod` with SIGPIPE and the
# pipeline reads as failed — a race that reports a loaded module as absent.
grep -q '^vivid ' /proc/modules || cannot_run "vivid module not loaded"
VIVID_DEVICE=""
while read -r dev; do
    if v4l2-ctl -d "$dev" --info 2>/dev/null | grep -q "Video Capture"; then
        VIVID_DEVICE="$dev"
        break
    fi
done < <(v4l2-ctl --list-devices 2>/dev/null | awk '/vivid/{getline; print $1}')
[ -n "$VIVID_DEVICE" ] || cannot_run "no vivid capture device found"
v4l2-ctl -d "$VIVID_DEVICE" --set-ctrl "test_pattern=$VIVID_TEST_PATTERN" \
    || fail "vivid would not take test_pattern=$VIVID_TEST_PATTERN"

SHAPED_INTERFACE="$(ip route show default | awk '{print $5; exit}')"
[ -n "$SHAPED_INTERFACE" ] || cannot_run "no default route to shape"

mkdir -p "$OUTPUT_DIR"

unshape() { sudo -n tc qdisc del dev "$SHAPED_INTERFACE" root >/dev/null 2>&1; }
trap unshape EXIT INT TERM

say "Interface:   $SHAPED_INTERFACE"
say "Shape:       rate $SHAPED_RATE, delay $SHAPED_DELAY, loss $SHAPED_LOSS"
say "Source:      $VIVID_DEVICE pattern $VIVID_TEST_PATTERN, video only, constant QP"
say "Arms:        baseline (no deadline) and policy (${DELIVERY_DEADLINE_MS} ms), ${ARM_SECONDS}s each"
say "Output:      $OUTPUT_DIR"

# ── One arm ──────────────────────────────────────────────────────────
run_one_arm() {
    arm_name="$1"
    shift
    log_file="$OUTPUT_DIR/$arm_name.log"

    unshape
    sudo -n tc qdisc add dev "$SHAPED_INTERFACE" root netem \
        delay "$SHAPED_DELAY" loss "$SHAPED_LOSS" rate "$SHAPED_RATE" \
        || fail "the shape would not apply to $SHAPED_INTERFACE"

    say "Running the $arm_name arm..."
    DISPLAY="${DISPLAY:-:0}" \
    RUST_LOG="${RUST_LOG:-warn,streamlib=info,streamlib_media_builtins=info}" \
        timeout --kill-after=5 "$ARM_SECONDS" \
            "$VENV_PYTHON" "$SCRIPT_DIR/moq_broadcast_roundtrip_node.py" \
                --camera "$VIVID_DEVICE" \
                --broadcast "streamlib/moq-drop-policy-$arm_name" \
                --control-plane-port "$CONTROL_PLANE_PORT" \
                --video-only \
                "$@" \
            > "$log_file" 2>&1
    node_status=$?
    unshape

    # The token never reaches the kept log, whatever any layer decided to print.
    if [ -n "${CLOUDFLARE_MOQ_PUB_SUB_TOKEN:-}" ]; then
        sed -i "s|$CLOUDFLARE_MOQ_PUB_SUB_TOKEN|<TOKEN>|g" "$log_file"
    fi
    if [ -n "${CLOUDFLARE_MOQ_SUB_TOKEN:-}" ]; then
        sed -i "s|$CLOUDFLARE_MOQ_SUB_TOKEN|<TOKEN>|g" "$log_file"
    fi

    # 124 is `timeout` ending a node that ran its whole budget. Anything else
    # is the node failing — and a node that fails can still log the
    # publisher's teardown on its way out, so the teardown line alone is not
    # proof the arm ran.
    [ "$node_status" -eq 124 ] || fail \
        "the $arm_name arm exited with status $node_status rather than running out its budget; see $log_file"
    grep -q "MoqBroadcastPublisher: teardown" "$log_file" \
        || fail "the $arm_name arm never reached the publisher's teardown; see $log_file"
}

report_one_arm() {
    arm_name="$1"
    log_file="$OUTPUT_DIR/$arm_name.log"
    echo
    say "── $arm_name ──"
    grep -hoE 'MoqBroadcastPublisher: (broadcast=.*|teardown, .*)' "$log_file" \
        | sed 's/ processor_id=.*//' | sed 's/^/    /'
    grep -hoE 'H264Encoder: teardown .*' "$log_file" | sed 's/^/    /'
    grep -hoE 'H264Decoder: teardown .*' "$log_file" | sed 's/^/    /'
}

run_one_arm baseline
run_one_arm policy --delivery-deadline-ms "$DELIVERY_DEADLINE_MS"

report_one_arm baseline
report_one_arm policy

echo
say "Both arms ran. The numbers above are the deliverable — read frames_decoded"
say "against frames_encoded, read frames_lost_to_gaps and"
say "frames_discarded_awaiting_a_sync_point as the decodability of what arrived,"
say "and read the policy arm's 'sheds begun on the backlog' and 'groups"
say "abandoned' as the uplink falling behind, seen from the publisher."
say "Logs: $OUTPUT_DIR"
