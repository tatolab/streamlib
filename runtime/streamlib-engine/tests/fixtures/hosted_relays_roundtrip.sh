#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# The hosted-relay live proof: the test pattern encoded on the rig, its public
# port forwarded by the runtime's publisher to a Cloudflare relay and to a
# moq.pro project, each read back off the hosted relay by a stock moq-dev
# client with a subscribe-only credential, decoded and scored; its private
# port read at the local relay's loopback listener and at neither hosted relay.
#
# Each provider is one arm, reported separately:
#   cloudflare  the relay Cloudflare provisioned, joined through `tatolab relay
#               add cloudflare`, read back with its subscribe-only token.
#   moqpro      the moq.pro project, joined through `tatolab relay add moqpro`,
#               read back with a subscribe-only JWT minted from the same key.
# An arm passes when `tatolab relays` lists its relay joined, the encoded track
# read off the hosted relay decodes to every frame read, each SMPTE bar's colour
# is within TOLERANCE of the colour rig's h264 baseline, and the private track
# does not read there. Cloudflare ships no web player for its MoQ relays, so a
# stock client reading the track is the playback proof.
#
# The lock is the decode-back: `tatolab-read-track`, ffmpeg and `cargo xtask
# psnr bar-colors`, the colour rig's path (`e2e_fixture_psnr_test_pattern.sh`),
# with the hosted relay inside it. A track that reads but scores off the
# baseline is the hosted relay's doing, since the rig scored the same encode at
# the local relay.
#
# CREDENTIALS. Each one is read from the environment — the repo-root `.env`
# sourced when a `STREAMLIB_` name is missing — and reaches the runtime only
# through `tatolab relay add --settings-file /dev/stdin`, a reader only through
# its stdin: never an argument, which `/proc` publishes, never a file. The
# runtime is started with no reachable session bus, so it keeps every secret in
# the owner-only fallback file of a state directory made for the run and
# removed with it, and never in the OS secret store, where a relay id this run
# assigns could name one of this user's own relays. The output directory is
# scanned for every credential however the run ends.
#
# Absent credentials make their arm a cannot-run, never a pass.
#
# Usage:
#   runtime/streamlib-engine/tests/fixtures/hosted_relays_roundtrip.sh [output_dir]
#
# Environment:
#   STREAMLIB_CLOUDFLARE_MOQ_PUBLISH_URL / STREAMLIB_CLOUDFLARE_MOQ_SUBSCRIBE_URL
#                             the relay's publish-and-subscribe and
#                             subscribe-only URLs. Fall back to
#                             `https://$CLOUDFLARE_MOQ_DRAFT_16_URL/` with
#                             CLOUDFLARE_MOQ_PUB_SUB_TOKEN /
#                             CLOUDFLARE_MOQ_SUB_TOKEN as the path.
#   STREAMLIB_MOQPRO_PROJECT_ID / STREAMLIB_MOQPRO_SIGNING_KEY
#                             the project and its JWK. Fall back to
#                             MOQPRO_PROJECT_ID or MOQPRO_PROJECT, and
#                             MOQPRO_JWK_JSON.
#   STREAMLIB_MOQPRO_RELAY_URL  the project's relays (default the adapter's,
#                             https://cdn.moq.dev)
#   ARMS                      the arms to run (default "cloudflare moqpro")
#   OBJECT_COUNT              encoded frames read back and measured (default 20)
#   JOIN_DEADLINE_SECONDS     how long a relay has to be listed joined (default 60)
#   READ_WAIT_SECONDS         how long a read back waits for its objects (default 60)
#   RUN_SECONDS               the sender's budget (default 240)
#   TOLERANCE                 abs per-bar channel drift bound (default 0.05)
#
# It measures whatever engine the runtime unit's `tatolabd` carries — rebuild
# it first: `cargo xtask build-runtime --release`. It refuses to run while
# another runtime holds the machine or answers at this user's local API socket.
#
# Exit codes: 0 = every arm passed, 1 = an arm failed, 77 = an arm could not
# run and none failed.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../../.." && pwd)"
BASELINE_TSV="$SCRIPT_DIR/psnr_test_pattern_bar_colors_baseline_h264.tsv"

OUTPUT_DIR="${1:-/tmp/streamlib-hosted-relays-$(date +%s)}"
ARMS="${ARMS:-cloudflare moqpro}"
OBJECT_COUNT="${OBJECT_COUNT:-20}"
JOIN_DEADLINE_SECONDS="${JOIN_DEADLINE_SECONDS:-60}"
READ_WAIT_SECONDS="${READ_WAIT_SECONDS:-60}"
RUN_SECONDS="${RUN_SECONDS:-240}"
TOLERANCE="${TOLERANCE:-0.05}"
# How long a read of the private track at a hosted relay is given to find it.
PRIVATE_TRACK_READ_WAIT_SECONDS=15

SENDER_STREAM_NAME="hosted_relays_sender"
PUBLIC_TRACK="encoder/encoded_video"
PRIVATE_TRACK="private_encoder/encoded_video"

say() { echo "[hosted-relays] $*"; }
cannot_run() { echo "[hosted-relays] SKIP: $*" >&2; exit 77; }
fail() { echo "[hosted-relays] FAIL: $*" >&2; exit 1; }

# ── What has to be here ──────────────────────────────────────────────
[ "$(uname -s)" = Linux ] || cannot_run \
    "Linux only: elsewhere the OS secret store cannot be kept out of the run"
for tool in cargo python3 ffmpeg; do
    command -v "$tool" >/dev/null || cannot_run "missing: $tool"
done
compgen -G "/dev/dri/renderD*" >/dev/null \
    || cannot_run "no DRM render node, so no GPU-backed runtime can start here"
[ -f "$BASELINE_TSV" ] || fail "no h264 bar-colour baseline at $BASELINE_TSV"

# shellcheck source=fixture_runtime_unit.sh
. "$SCRIPT_DIR/fixture_runtime_unit.sh"
require_the_runtime_unit
require_the_fixture_venv
refuse_while_a_runtime_answers_at_the_local_api_socket \
    || cannot_run "a runtime already holds this machine; this run starts its own"

# ── The credentials, from the environment and never from the tree ────
if { [ -z "${STREAMLIB_CLOUDFLARE_MOQ_PUBLISH_URL:-}" ] \
        || [ -z "${STREAMLIB_MOQPRO_SIGNING_KEY:-}" ]; } && [ -f "$REPO_ROOT/.env" ]; then
    set -a
    # shellcheck disable=SC1091
    . "$REPO_ROOT/.env" >/dev/null 2>&1 || true
    set +a
fi
the_cloudflare_relay_url_with_the_path() {
    local relay_host="${CLOUDFLARE_MOQ_DRAFT_16_URL:-}" path="$1"
    [ -n "$relay_host" ] && [ -n "$path" ] || return 0
    relay_host="${relay_host#https://}"
    printf 'https://%s/%s' "${relay_host%/}" "$path"
}
: "${STREAMLIB_CLOUDFLARE_MOQ_PUBLISH_URL:=$(the_cloudflare_relay_url_with_the_path "${CLOUDFLARE_MOQ_PUB_SUB_TOKEN:-}")}"
: "${STREAMLIB_CLOUDFLARE_MOQ_SUBSCRIBE_URL:=$(the_cloudflare_relay_url_with_the_path "${CLOUDFLARE_MOQ_SUB_TOKEN:-}")}"
: "${STREAMLIB_MOQPRO_PROJECT_ID:=${MOQPRO_PROJECT_ID:-${MOQPRO_PROJECT:-}}}"
: "${STREAMLIB_MOQPRO_SIGNING_KEY:=${MOQPRO_JWK_JSON:-}}"
: "${STREAMLIB_MOQPRO_RELAY_URL:=}"

declare -A ARM_OUTCOME=()
declare -A ARM_REASON=()
declare -A ARM_RELAY_ID=()
arm_cannot_run() { ARM_OUTCOME[$1]="cannot-run"; ARM_REASON[$1]="$2"; }
arm_failed() { ARM_OUTCOME[$1]="fail"; ARM_REASON[$1]="$2"; say "$1: FAIL: $2" >&2; }

ARMS_TO_RUN=()
for arm in $ARMS; do
    case "$arm" in
        cloudflare)
            if [ -n "$STREAMLIB_CLOUDFLARE_MOQ_PUBLISH_URL" ] \
                && [ -n "$STREAMLIB_CLOUDFLARE_MOQ_SUBSCRIBE_URL" ]; then
                ARMS_TO_RUN+=(cloudflare)
            else
                arm_cannot_run cloudflare "no Cloudflare relay URLs: export \
STREAMLIB_CLOUDFLARE_MOQ_PUBLISH_URL and STREAMLIB_CLOUDFLARE_MOQ_SUBSCRIBE_URL, or put \
CLOUDFLARE_MOQ_DRAFT_16_URL, CLOUDFLARE_MOQ_PUB_SUB_TOKEN and CLOUDFLARE_MOQ_SUB_TOKEN in the repo-root .env"
            fi
            ;;
        moqpro)
            if [ -n "$STREAMLIB_MOQPRO_PROJECT_ID" ] && [ -n "$STREAMLIB_MOQPRO_SIGNING_KEY" ]; then
                ARMS_TO_RUN+=(moqpro)
            else
                arm_cannot_run moqpro "no moq.pro project: export STREAMLIB_MOQPRO_PROJECT_ID and \
STREAMLIB_MOQPRO_SIGNING_KEY, or put MOQPRO_PROJECT_ID and MOQPRO_JWK_JSON in the repo-root .env"
            fi
            ;;
        *) fail "ARMS names '$arm'; the arms are cloudflare and moqpro" ;;
    esac
done
export STREAMLIB_CLOUDFLARE_MOQ_PUBLISH_URL STREAMLIB_CLOUDFLARE_MOQ_SUBSCRIBE_URL \
    STREAMLIB_MOQPRO_PROJECT_ID STREAMLIB_MOQPRO_SIGNING_KEY STREAMLIB_MOQPRO_RELAY_URL

report_every_arm() {
    local arm exit_status=0
    echo ""
    for arm in $ARMS; do
        say "$(printf '%-11s %-11s %s' "$arm" "${ARM_OUTCOME[$arm]:-fail}" "${ARM_REASON[$arm]:-it did not finish}")"
        case "${ARM_OUTCOME[$arm]:-fail}" in
            pass) ;;
            cannot-run) [ "$exit_status" -eq 1 ] || exit_status=77 ;;
            *) exit_status=1 ;;
        esac
    done
    return "$exit_status"
}
[ "${#ARMS_TO_RUN[@]}" -gt 0 ] || { report_every_arm; exit $?; }

mkdir -p "$OUTPUT_DIR"
LOG_FILE="$OUTPUT_DIR/runtime.log"
SENDER_RUN_LOG_FILE="$OUTPUT_DIR/sender_run.log"
# Outside the output directory and removed on exit: it holds the relay
# secrets' fallback file.
RUNTIME_STATE_HOME="$(mktemp -d "${TMPDIR:-/tmp}/streamlib-hosted-relays-runtime-state-XXXXXX")"

SENDER_RUN_PID=""
SENDER_RUN_NEEDED_SIGKILL=0
stop_the_sender() {
    [ -n "$SENDER_RUN_PID" ] || return 0
    if kill -0 "$SENDER_RUN_PID" 2>/dev/null; then
        kill -TERM "$SENDER_RUN_PID" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$SENDER_RUN_PID" 2>/dev/null || break
            sleep 0.2
        done
        if kill -0 "$SENDER_RUN_PID" 2>/dev/null; then
            SENDER_RUN_NEEDED_SIGKILL=1
            kill -9 "$SENDER_RUN_PID" 2>/dev/null || true
        fi
    fi
    wait "$SENDER_RUN_PID" 2>/dev/null || true
    SENDER_RUN_PID=""
}
# Every credential this run holds, the signing key's private material among
# them, looked for in every file under the output directory. Read from the
# environment, so no credential is an argument of the scan.
refuse_a_credential_written_into_the_output_directory() {
    [ -d "$OUTPUT_DIR" ] || return 0
    if ! "$FIXTURE_PYTHON" -I -c '
import json, os, pathlib, sys
credentials = [
    os.environ.get(name, "")
    for name in (
        "STREAMLIB_CLOUDFLARE_MOQ_PUBLISH_URL",
        "STREAMLIB_CLOUDFLARE_MOQ_SUBSCRIBE_URL",
        "STREAMLIB_MOQPRO_SIGNING_KEY",
    )
]
for url in credentials[:2]:
    credentials.append(url.rstrip("/").rsplit("/", 1)[-1] if url else "")
try:
    signing_key = json.loads(os.environ.get("STREAMLIB_MOQPRO_SIGNING_KEY") or "{}")
    credentials += [signing_key.get(field, "") for field in ("d", "k", "p", "q")]
except ValueError:
    pass
credentials = [credential for credential in credentials if len(credential) >= 8]
credentials.append("jwt=")
found = [
    str(path)
    for path in pathlib.Path(sys.argv[1]).rglob("*")
    if path.is_file()
    and any(credential.encode() in path.read_bytes() for credential in credentials)
]
if found:
    print("\n".join(found))
    sys.exit(1)
' "$OUTPUT_DIR"; then
        echo "[hosted-relays] FAIL: a credential was written into the files above under" \
            "$OUTPUT_DIR; delete the directory before sharing any of it" >&2
        exit 1
    fi
}
forget_the_relays_and_stop() {
    local exit_status=$?
    stop_the_sender
    local arm
    for arm in "${!ARM_RELAY_ID[@]}"; do
        "$TATOLAB_EXECUTABLE" relay rm "${ARM_RELAY_ID[$arm]}" >/dev/null 2>&1 || true
    done
    stop_the_fixture_runtime
    rm -rf "$RUNTIME_STATE_HOME"
    refuse_a_credential_written_into_the_output_directory
    exit "$exit_status"
}
trap forget_the_relays_and_stop EXIT

say "Output dir:        $OUTPUT_DIR"
say "Arms:              ${ARMS_TO_RUN[*]}"
say "Credentials:       <redacted — each relay URL and the signing key are secrets>"

# ── Build ────────────────────────────────────────────────────────────
cd "$REPO_ROOT" || fail "cannot enter $REPO_ROOT"
say "Building tatolab-read-track, xtask and the moq.pro subscribe minter (release)..."
require_tatolab_read_track
cargo build --release --locked -p xtask \
    -p streamlib-relay-adapter-moqpro --example mint_a_moqpro_subscribe_url \
    > "$OUTPUT_DIR/build.log" 2>&1 \
    || { tail -40 "$OUTPUT_DIR/build.log" >&2; fail "the scorer or the minter did not build"; }
MOQPRO_SUBSCRIBE_URL_MINTER="$REPO_ROOT/target/release/examples/mint_a_moqpro_subscribe_url"

# ── The runtime, with no OS secret store ─────────────────────────────
export RUST_LOG="${RUST_LOG:-warn,tatolabd=info,streamlib=info,streamlib_media_builtins=info}"
DBUS_SESSION_BUS_ADDRESS="unix:path=$RUNTIME_STATE_HOME/no-session-bus" \
    start_the_fixture_runtime "$LOG_FILE" "$RUNTIME_STATE_HOME" \
    || fail "the fixture could not start its runtime"
grep -q "no OS secret store answers" "$LOG_FILE" \
    || cannot_run "the runtime reached an OS secret store, which this run keeps out; nothing was added to it"

MACHINE_NAME="$(the_machine_name_the_loopback_listing_names)" \
    || fail "the runtime's loopback listing did not name the machine"
SENDER_BROADCAST="$MACHINE_NAME/$SENDER_STREAM_NAME.hang"

# ── Each hosted relay, added and joined ──────────────────────────────
# Prints the relay id `tatolab relay add <adapter>` gives the settings the
# Python on stdin writes from the environment.
add_the_relay() {
    local adapter="$1" settings_from_the_environment="$2" added
    added="$("$FIXTURE_PYTHON" -I -c "import json, os; print(json.dumps($settings_from_the_environment))" \
        | "$TATOLAB_EXECUTABLE" relay add "$adapter" --settings-file /dev/stdin 2>"$OUTPUT_DIR/relay_add_$adapter.log")" \
        || return 1
    printf '%s' "${added%% *}"
}
# The state `tatolab relays` lists for <relay id>.
the_state_listed_for() {
    "$TATOLAB_EXECUTABLE" relays 2>/dev/null | awk -v relay_id="$1" '$1 == relay_id { print $3 }'
}

for arm in "${ARMS_TO_RUN[@]}"; do
    case "$arm" in
        cloudflare)
            relay_id="$(add_the_relay cloudflare \
                '{"relay_url": os.environ["STREAMLIB_CLOUDFLARE_MOQ_PUBLISH_URL"]}')" ;;
        moqpro)
            relay_id="$(add_the_relay moqpro \
                '{key: value for key, value in {"project_id": os.environ["STREAMLIB_MOQPRO_PROJECT_ID"], "signing_key": os.environ["STREAMLIB_MOQPRO_SIGNING_KEY"], "relay_url": os.environ["STREAMLIB_MOQPRO_RELAY_URL"] or None}.items() if value is not None}')" ;;
    esac
    if [ -z "$relay_id" ]; then
        arm_failed "$arm" "tatolab relay add refused: $(cat "$OUTPUT_DIR/relay_add_$arm.log")"
        continue
    fi
    ARM_RELAY_ID[$arm]="$relay_id"
    say "$arm: added as      $relay_id"
done
"$TATOLAB_EXECUTABLE" relays > "$OUTPUT_DIR/relays_after_adding.txt" 2>&1 || true

# ── The sender ───────────────────────────────────────────────────────
timeout --kill-after=5 "$RUN_SECONDS" \
    "$TATOLAB_EXECUTABLE" run \
        --dir "$SCRIPT_DIR" \
        --name "$SENDER_STREAM_NAME" \
        "hosted_relays_roundtrip_stream.py:the_test_pattern_encoded_with_one_port_public_and_one_private" \
    > "$SENDER_RUN_LOG_FILE" 2>&1 &
SENDER_RUN_PID=$!
if ! wait_until_the_stream_is_running "$SENDER_STREAM_NAME" "$SENDER_RUN_PID" 60; then
    tail -30 "$SENDER_RUN_LOG_FILE" >&2
    tail -30 "$LOG_FILE" >&2
    fail "the sender never ran — it never answered over the local API socket with every node Running"
fi
say "Running:           $SENDER_STREAM_NAME, broadcast $SENDER_BROADCAST"

# The positive control: the private track is written, and reads at loopback.
if ! read_the_track_at_the_local_relay "$SENDER_BROADCAST" "$PRIVATE_TRACK" 3 \
        "$OUTPUT_DIR/private_track_at_loopback" 2> "$OUTPUT_DIR/private_track_at_loopback.log"; then
    cat "$OUTPUT_DIR/private_track_at_loopback.log" >&2
    fail "the private track does not read at the local relay's loopback listener, so its absence at a hosted relay would prove nothing"
fi
say "Private track:     reads at the loopback listener"

# Reads <count> objects of <track> into <directory> at the relay whose URL,
# carrying its credential, is on stdin.
read_the_track_at_the_relay_on_stdin() {
    local track="$1" object_count="$2" dump_directory="$3" wait_at_most="$4"
    "$TATOLAB_READ_TRACK_EXECUTABLE" - "$SENDER_BROADCAST" "$track" \
        --count "$object_count" --out "$dump_directory" --wait-at-most "$wait_at_most"
}
# Writes the subscribe-only URL of <arm>'s relay to stdout.
the_subscribe_url_of() {
    case "$1" in
        cloudflare) printf '%s' "$STREAMLIB_CLOUDFLARE_MOQ_SUBSCRIBE_URL" ;;
        moqpro)
            local -a relay_url_argument=()
            [ -z "$STREAMLIB_MOQPRO_RELAY_URL" ] || relay_url_argument=("$STREAMLIB_MOQPRO_RELAY_URL")
            printf '%s' "$STREAMLIB_MOQPRO_SIGNING_KEY" \
                | "$MOQPRO_SUBSCRIBE_URL_MINTER" "$STREAMLIB_MOQPRO_PROJECT_ID" \
                    "$SENDER_BROADCAST" "${relay_url_argument[@]}"
            ;;
    esac
}

prove_the_arm() {
    local arm="$1" relay_id="${ARM_RELAY_ID[$1]}" arm_dir="$OUTPUT_DIR/$1" state="" subscribe_url
    mkdir -p "$arm_dir"
    for _ in $(seq 1 $(( JOIN_DEADLINE_SECONDS * 2 ))); do
        state="$(the_state_listed_for "$relay_id")"
        [ "$state" = joined ] && break
        sleep 0.5
    done
    "$TATOLAB_EXECUTABLE" relays > "$arm_dir/relays.txt" 2>&1 || true
    if [ "$state" != joined ]; then
        arm_failed "$arm" "$relay_id is listed '${state:-absent}', not joined, after ${JOIN_DEADLINE_SECONDS}s: $(grep "^$relay_id is" "$arm_dir/relays.txt")"
        return
    fi
    say "$arm: $relay_id joined; the publisher's session is connected"

    subscribe_url="$(the_subscribe_url_of "$arm" 2> "$arm_dir/subscribe_url.log")" \
        || { arm_failed "$arm" "no subscribe URL: $(cat "$arm_dir/subscribe_url.log")"; return; }
    if ! printf '%s' "$subscribe_url" | read_the_track_at_the_relay_on_stdin "$PUBLIC_TRACK" \
            "$OBJECT_COUNT" "$arm_dir/encoded_track" "$READ_WAIT_SECONDS" 2> "$arm_dir/read_track.log"; then
        arm_failed "$arm" "the public track did not read off the hosted relay: $(tail -3 "$arm_dir/read_track.log")"
        return
    fi
    say "$arm: $(tail -1 "$arm_dir/read_track.log")"
    # Without the catalog the reader writes each object as sent: the legacy
    # container, a QUIC varint timestamp before the codec payload. The rest of
    # the arm strips it here, so one gap does not hide what else holds.
    local why_the_catalog_failed=""
    if ! grep -q '"payload":"codec_payload_of_the_hang_legacy_container"' \
            "$arm_dir/encoded_track/objects.jsonl"; then
        local catalog_track_read="no object of it arrived within ${PRIVATE_TRACK_READ_WAIT_SECONDS}s"
        if printf '%s' "$(the_subscribe_url_of "$arm" 2>/dev/null)" \
                | read_the_track_at_the_relay_on_stdin catalog.json 1 "$arm_dir/catalog_track" \
                    "$PRIVATE_TRACK_READ_WAIT_SECONDS" 2> "$arm_dir/catalog_track_read.log"; then
            catalog_track_read="it read as a track when subscribed to directly"
        fi
        why_the_catalog_failed="the track read back as sent, not as its codec payload: the broadcast's catalog did not reach the reader within 5s ($catalog_track_read)"
        say "$arm: FINDING: $why_the_catalog_failed"
    fi

    "$FIXTURE_PYTHON" -c '
import json, pathlib, sys
dump = pathlib.Path(sys.argv[1])
with open(sys.argv[2], "wb") as elementary_stream:
    for line in (dump / "objects.jsonl").read_text().splitlines():
        record = json.loads(line)
        payload = (dump / record["file"]).read_bytes()
        if record["payload"] == "as_sent":
            payload = payload[1 << (payload[0] >> 6):]
        elementary_stream.write(payload)
' "$arm_dir/encoded_track" "$arm_dir/encoded.h264"
    mkdir -p "$arm_dir/measured"
    if ! ffmpeg -hide_banner -loglevel error -nostdin -f h264 -i "$arm_dir/encoded.h264" \
            -fps_mode passthrough -pix_fmt rgb24 "$arm_dir/measured/%04d.png" 2> "$arm_dir/ffmpeg.log"; then
        arm_failed "$arm" "ffmpeg could not decode the track read back: $(tail -3 "$arm_dir/ffmpeg.log")"
        return
    fi
    local decoded_png_count
    decoded_png_count="$(find "$arm_dir/measured" -name '*.png' | wc -l)"
    if [ "$decoded_png_count" -ne "$OBJECT_COUNT" ]; then
        arm_failed "$arm" "ffmpeg decoded $decoded_png_count of the $OBJECT_COUNT frames read back"
        return
    fi
    if ! "$REPO_ROOT/target/release/xtask" psnr bar-colors --images "$arm_dir/measured" \
            --baseline "$BASELINE_TSV" --tolerance "$TOLERANCE" \
            --report "$arm_dir/bar_colors.tsv" > "$arm_dir/bar_colors.log" 2>&1; then
        arm_failed "$arm" "a bar's colour drifted past $TOLERANCE of the baseline: $(tail -3 "$arm_dir/bar_colors.log")"
        return
    fi
    say "$arm: $decoded_png_count frames decoded, every bar within $TOLERANCE of the baseline"

    subscribe_url="$(the_subscribe_url_of "$arm" 2>/dev/null)"
    if printf '%s' "$subscribe_url" | read_the_track_at_the_relay_on_stdin "$PRIVATE_TRACK" 1 \
            "$arm_dir/private_track" "$PRIVATE_TRACK_READ_WAIT_SECONDS" \
            2> "$arm_dir/private_track_read.log"; then
        arm_failed "$arm" "the private track read off the hosted relay"
        return
    fi
    say "$arm: the private track does not read there"
    if [ -n "$why_the_catalog_failed" ]; then
        arm_failed "$arm" "$why_the_catalog_failed; the media read back decoded and scored within $TOLERANCE, and the private track is absent"
        return
    fi
    ARM_OUTCOME[$arm]="pass"
    ARM_REASON[$arm]="joined, read back, decoded, scored; private track absent"
}

for arm in "${ARMS_TO_RUN[@]}"; do
    [ -n "${ARM_RELAY_ID[$arm]:-}" ] || continue
    prove_the_arm "$arm"
done

stop_the_sender
[ "$SENDER_RUN_NEEDED_SIGKILL" -eq 0 ] \
    || fail "the sender did not exit on SIGTERM and needed SIGKILL — a hung teardown, not a slow exit"
for arm in "${!ARM_RELAY_ID[@]}"; do
    "$TATOLAB_EXECUTABLE" relay rm "${ARM_RELAY_ID[$arm]}" >/dev/null 2>&1 \
        || say "$arm: ${ARM_RELAY_ID[$arm]} was not removed"
done
ARM_RELAY_ID=()
stop_the_fixture_runtime 15
the_fixture_runtime_stopped_cleanly \
    || fail "the runtime did not stop cleanly once its stream was unloaded"

say "Output dir:        $OUTPUT_DIR"
report_every_arm
