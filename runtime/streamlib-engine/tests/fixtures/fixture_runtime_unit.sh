# shellcheck shell=bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Sourced by the fixture drivers: the runtime unit a fixture stream runs on,
# the venv it compiles in, the runtime a fixture starts and stops itself, the
# runtime unit's `tatolab` verbs that load and observe a stream, the stream runs
# a driver starts and stops, and `tatolab-read-track`, which reads a stream's
# exposed port as a track at the machine's local relay, as any user reads it,
# with the decode of what it wrote.
#
# The runtime unit is what `cargo xtask build-runtime` lays out —
# `bin/tatolab`, `bin/tatolabd` and `lib/tatolab/lend/` — at
# `$STREAMLIB_RUNTIME_UNIT_DIRECTORY`, else `<repository>/target/tatolab-runtime`.
# A driver measures that unit, so rebuild it before a run that should see an
# edit to the engine. A unit built with `--machine-directories-under-a-test-root`
# is the integration suite's, and is refused.
#
# A fixture starts its own `tatolabd`, which takes the machine's real runtime
# lock and serves this user's local API socket, so it refuses to run while
# another runtime holds the machine. It loads its stream with `tatolab run
# --name`, addresses it by that name, and stops the run and then the runtime.
#
# The fixture venv is `<this directory>/.venv`, because `tatolab run --dir`
# has the runtime compile a stream in its project's `.venv`. It holds
# `tatolab-stream` from this checkout, editable, and numpy, which the fixture
# nodes import; never the runtime, which a processor interpreter borrows from
# the lend. It is made with uv the first time a driver needs it; delete the
# directory to have it made again. To use a venv prepared elsewhere instead,
# name it with STREAMLIB_FIXTURE_VENV: it is checked the same way and
# `<this directory>/.venv` becomes a symlink to it, which stays in use until
# deleted.

FIXTURE_DIRECTORY="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURE_REPOSITORY_ROOT="$(cd "$FIXTURE_DIRECTORY/../../../.." && pwd)"
RUNTIME_UNIT_DIRECTORY="${STREAMLIB_RUNTIME_UNIT_DIRECTORY:-$FIXTURE_REPOSITORY_ROOT/target/tatolab-runtime}"
TATOLAB_EXECUTABLE="$RUNTIME_UNIT_DIRECTORY/bin/tatolab"
TATOLABD_EXECUTABLE="$RUNTIME_UNIT_DIRECTORY/bin/tatolabd"
RUNTIME_UNIT_LEND_DIRECTORY="$RUNTIME_UNIT_DIRECTORY/lib/tatolab/lend"
FIXTURE_VENV_DIRECTORY="$FIXTURE_DIRECTORY/.venv"
FIXTURE_PYTHON="$FIXTURE_VENV_DIRECTORY/bin/python"
# The file `cargo xtask build-runtime --machine-directories-under-a-test-root`
# leaves at the root of a unit whose binaries refuse to run without
# TATOLAB_TEST_MACHINE_ROOT.
TEST_MACHINE_ROOT_RUNTIME_UNIT_MARKER="$RUNTIME_UNIT_DIRECTORY/machine-directories-under-a-test-root"

# Exits 77, naming what is missing, unless the runtime unit can start a stream.
require_the_runtime_unit() {
    local runtime_unit_member
    for runtime_unit_member in "$TATOLAB_EXECUTABLE" "$TATOLABD_EXECUTABLE"; do
        if [ ! -x "$runtime_unit_member" ]; then
            echo "SKIP: no $runtime_unit_member — build the runtime unit with" \
                "\`cargo xtask build-runtime\`, or name one with STREAMLIB_RUNTIME_UNIT_DIRECTORY" >&2
            exit 77
        fi
    done
    if [ ! -f "$RUNTIME_UNIT_LEND_DIRECTORY/tatolab/runtime/__init__.py" ]; then
        echo "SKIP: the runtime unit at $RUNTIME_UNIT_DIRECTORY has no lend — rebuild it with" \
            "\`cargo xtask build-runtime\`" >&2
        exit 77
    fi
    if [ -e "$TEST_MACHINE_ROOT_RUNTIME_UNIT_MARKER" ]; then
        echo "SKIP: the runtime unit at $RUNTIME_UNIT_DIRECTORY is the integration suite's" \
            "(it carries $TEST_MACHINE_ROOT_RUNTIME_UNIT_MARKER), whose binaries run only under a" \
            "test machine root — rebuild it with \`cargo xtask build-runtime\`" >&2
        exit 77
    fi
}

# Makes the fixture venv when it is absent, then exits 77 unless it can compile
# a fixture stream, and 1 if it carries a runtime of its own.
require_the_fixture_venv() {
    if [ -n "${STREAMLIB_FIXTURE_VENV:-}" ]; then
        local prepared_fixture_venv_directory
        if ! prepared_fixture_venv_directory="$(cd "$STREAMLIB_FIXTURE_VENV" 2>/dev/null && pwd -P)"; then
            echo "ERROR: STREAMLIB_FIXTURE_VENV names $STREAMLIB_FIXTURE_VENV, which is not a directory" >&2
            exit 1
        fi
        if [ -e "$FIXTURE_VENV_DIRECTORY" ] && [ ! -L "$FIXTURE_VENV_DIRECTORY" ]; then
            echo "ERROR: STREAMLIB_FIXTURE_VENV is set, but $FIXTURE_VENV_DIRECTORY is a venv of its" \
                "own — delete it to run on $prepared_fixture_venv_directory" >&2
            exit 1
        fi
        ln -sfn "$prepared_fixture_venv_directory" "$FIXTURE_VENV_DIRECTORY"
    fi
    if [ ! -x "$FIXTURE_PYTHON" ]; then
        if ! command -v uv >/dev/null 2>&1; then
            echo "SKIP: no fixture venv at $FIXTURE_VENV_DIRECTORY, and no uv to make one" >&2
            exit 77
        fi
        echo "making the fixture venv at $FIXTURE_VENV_DIRECTORY" >&2
        if ! uv venv --quiet --python 3.12 "$FIXTURE_VENV_DIRECTORY" >&2 \
            || ! VIRTUAL_ENV="$FIXTURE_VENV_DIRECTORY" uv pip install --quiet \
                -e "$FIXTURE_REPOSITORY_ROOT/sdk/tatolab-stream" 'numpy>=2.1' >&2; then
            echo "ERROR: could not make the fixture venv at $FIXTURE_VENV_DIRECTORY" >&2
            exit 1
        fi
    fi
    if ! "$FIXTURE_PYTHON" -c 'import numpy, tatolab.stream' >/dev/null 2>&1; then
        echo "SKIP: $FIXTURE_PYTHON cannot import tatolab.stream and numpy — delete" \
            "$FIXTURE_VENV_DIRECTORY to have it made again, or fix the venv STREAMLIB_FIXTURE_VENV names" >&2
        exit 77
    fi
    if "$FIXTURE_PYTHON" -c '
import importlib.util, sys
sys.exit(0 if importlib.util.find_spec("tatolab.runtime") is not None else 1)
' 2>/dev/null; then
        echo "ERROR: $FIXTURE_VENV_DIRECTORY carries tatolab.runtime, so a fixture stream would" \
            "not be running from a venv holding only tatolab-stream and its own dependencies" >&2
        exit 1
    fi
}


# Runs a verb of the runtime unit's `tatolab` (`graph`, `logs`, `streams`,
# `expose`).
tatolab_observation_verb() {
    "$TATOLAB_EXECUTABLE" "$@"
}

# The machine's local relay at its loopback listener, which serves every
# private and public port; and the runtime's loopback HTTP listing.
LOCAL_RELAY_LOOPBACK_URL="http://127.0.0.1:4443"
LOOPBACK_HTTP_LISTING_URL="http://127.0.0.1:4446/"
TATOLAB_READ_TRACK_EXECUTABLE="$FIXTURE_REPOSITORY_ROOT/target/release/tatolab-read-track"

# Builds `tatolab-read-track` from this checkout, exiting 1 when it does not
# build. A development tool, never in the runtime unit. Once built, it says so
# in the environment, so a driver it calls while a stream runs builds nothing.
require_tatolab_read_track() {
    [ "${TATOLAB_READ_TRACK_IS_BUILT:-}" = 1 ] && return 0
    if ! (cd "$FIXTURE_REPOSITORY_ROOT" \
        && cargo build --release --locked --quiet -p tatolab-read-track) >&2; then
        echo "ERROR: tatolab-read-track does not build" >&2
        exit 1
    fi
    export TATOLAB_READ_TRACK_IS_BUILT=1
}

# Prints the machine's name as the runtime's loopback listing names it.
the_machine_name_the_loopback_listing_names() {
    "$FIXTURE_PYTHON" -c '
import json, sys, urllib.request
with urllib.request.urlopen(sys.argv[1], timeout=5) as listing:
    print(json.load(listing)["machine"])
' "$LOOPBACK_HTTP_LISTING_URL"
}

# Whether the runtime's loopback listing names <stream>'s port <node>/<port>
# among those it exposes.
the_loopback_listing_exposes() {
    "$FIXTURE_PYTHON" -c '
import json, sys, urllib.request
listing_url, stream_name, node_name, port_name = sys.argv[1:]
with urllib.request.urlopen(listing_url, timeout=5) as listing:
    streams = json.load(listing)["streams"]
sys.exit(0 if any(
    stream["name"] == stream_name
    and any(
        exposed["node"] == node_name and exposed["port"] == port_name
        for exposed in stream["exposed_output_ports"]
    )
    for stream in streams
) else 1)
' "$LOOPBACK_HTTP_LISTING_URL" "$1" "$2" "$3"
}

# Reads <count> objects of <track> in <broadcast> at the local relay's loopback
# listener into <directory>, as `tatolab-read-track` writes them.
read_the_track_at_the_local_relay() {
    local broadcast="$1" track="$2" object_count="$3" dump_directory="$4"
    "$TATOLAB_READ_TRACK_EXECUTABLE" "$LOCAL_RELAY_LOOPBACK_URL" "$broadcast" "$track" \
        --count "$object_count" --out "$dump_directory"
}

# Whether a runtime answers at this user's local API socket now.
a_runtime_answers_at_the_local_api_socket() {
    "$TATOLAB_EXECUTABLE" streams >/dev/null 2>&1
}

# Returns 1, saying so, when a runtime already answers at this user's local
# API socket: a Rust rig serves that socket itself, and refuses it held.
refuse_while_a_runtime_answers_at_the_local_api_socket() {
    if a_runtime_answers_at_the_local_api_socket; then
        echo "REFUSED: a runtime already answers at this user's local API socket; stop it" \
            "(Ctrl-C in its terminal) and run the fixture again — a fixture starts its own:" >&2
        "$TATOLAB_EXECUTABLE" streams >&2 || true
        return 1
    fi
}

FIXTURE_RUNTIME_PID=""
# How the fixture's runtime ended: never-started, stopped-cleanly,
# needed-sigkill or already-gone, with its exit status beside it.
FIXTURE_RUNTIME_STOP_OUTCOME="never-started"
FIXTURE_RUNTIME_EXIT_STATUS=""

# Starts the runtime unit's `tatolabd` in the background as
# FIXTURE_RUNTIME_PID, its output — the engine's log mirror, every stream's
# records among it — in <log file>, and returns once its local API answers.
# Every stream setting a fixture passes in the environment goes here: the
# runtime compiles a stream and starts its processor interpreters with its own
# environment, never `tatolab run`'s.
#
# Its state directory sits under <state home>: XDG_STATE_HOME names it on
# Linux, and HOME on macOS, where the state directory is `$HOME/Library/
# Application Support/Tatolab`. So it re-loads none of this user's recorded streams
# and leaves its own log beside the fixture's evidence. The runtime directory
# and the lock read neither variable, so the local API socket stays where
# `tatolab` looks. It takes the machine's real runtime lock: when another
# runtime holds the machine, `tatolabd` refuses naming the holder, and this
# returns 1 having printed that refusal.
start_the_fixture_runtime() {
    local runtime_log_file="$1" runtime_state_home="$2"
    local a_runtime_answered_before_the_start=0 runtime_exit_status
    local -a state_directory_environment=("XDG_STATE_HOME=$runtime_state_home")
    if [ "$(uname -s)" = Darwin ]; then
        state_directory_environment+=("HOME=$runtime_state_home")
    fi
    if a_runtime_answers_at_the_local_api_socket; then
        a_runtime_answered_before_the_start=1
    fi
    mkdir -p "$runtime_state_home"
    env "${state_directory_environment[@]}" "$TATOLABD_EXECUTABLE" >"$runtime_log_file" 2>&1 &
    FIXTURE_RUNTIME_PID=$!
    FIXTURE_RUNTIME_STOP_OUTCOME="never-started"
    FIXTURE_RUNTIME_EXIT_STATUS=""
    for _ in $(seq 1 120); do
        if ! kill -0 "$FIXTURE_RUNTIME_PID" 2>/dev/null; then
            runtime_exit_status=0
            wait "$FIXTURE_RUNTIME_PID" 2>/dev/null || runtime_exit_status=$?
            FIXTURE_RUNTIME_PID=""
            FIXTURE_RUNTIME_STOP_OUTCOME="already-gone"
            FIXTURE_RUNTIME_EXIT_STATUS="$runtime_exit_status"
            echo "REFUSED: the fixture's runtime $TATOLABD_EXECUTABLE exited" \
                "$runtime_exit_status before it served its local API:" >&2
            tail -20 "$runtime_log_file" >&2
            if grep -q "another runtime holds this machine" "$runtime_log_file"; then
                echo "Stop the machine's runtime (Ctrl-C in its terminal) and run the fixture" \
                    "again — a fixture starts its own." >&2
            fi
            return 1
        fi
        # A socket that answered before this runtime started is someone
        # else's; this runtime then refuses the lock or the socket and exits.
        if [ "$a_runtime_answered_before_the_start" -eq 0 ] \
            && a_runtime_answers_at_the_local_api_socket; then
            return 0
        fi
        sleep 0.25
    done
    echo "REFUSED: the fixture's runtime did not answer at the local API socket within 30 s:" >&2
    tail -20 "$runtime_log_file" >&2
    stop_the_fixture_runtime
    return 1
}

# SIGINT to the fixture's runtime, then up to <seconds> (default 15) for it to
# exit before SIGKILL. Sets FIXTURE_RUNTIME_STOP_OUTCOME and
# FIXTURE_RUNTIME_EXIT_STATUS; a second call leaves them as the first set them.
stop_the_fixture_runtime() {
    local stop_budget_seconds="${1:-15}"
    [ -n "$FIXTURE_RUNTIME_PID" ] || return 0
    if ! kill -0 "$FIXTURE_RUNTIME_PID" 2>/dev/null; then
        FIXTURE_RUNTIME_STOP_OUTCOME="already-gone"
    else
        kill -INT "$FIXTURE_RUNTIME_PID" 2>/dev/null || true
        for _ in $(seq 1 $(( stop_budget_seconds * 5 ))); do
            kill -0 "$FIXTURE_RUNTIME_PID" 2>/dev/null || break
            sleep 0.2
        done
        if kill -0 "$FIXTURE_RUNTIME_PID" 2>/dev/null; then
            FIXTURE_RUNTIME_STOP_OUTCOME="needed-sigkill"
            kill -KILL "$FIXTURE_RUNTIME_PID" 2>/dev/null || true
        else
            FIXTURE_RUNTIME_STOP_OUTCOME="stopped-cleanly"
        fi
    fi
    FIXTURE_RUNTIME_EXIT_STATUS=0
    wait "$FIXTURE_RUNTIME_PID" 2>/dev/null || FIXTURE_RUNTIME_EXIT_STATUS=$?
    FIXTURE_RUNTIME_PID=""
}

# Returns 0 when the fixture's runtime stopped cleanly — it took the SIGINT and
# exited 0 inside the budget — and 1, saying how it did not, otherwise.
the_fixture_runtime_stopped_cleanly() {
    if [ "$FIXTURE_RUNTIME_STOP_OUTCOME" = stopped-cleanly ] \
        && [ "$FIXTURE_RUNTIME_EXIT_STATUS" = 0 ]; then
        return 0
    fi
    case "$FIXTURE_RUNTIME_STOP_OUTCOME" in
        stopped-cleanly)
            echo "the fixture's runtime exited $FIXTURE_RUNTIME_EXIT_STATUS on SIGINT, not 0" \
                "(124 is a stream teardown its watchdog abandoned)" >&2 ;;
        needed-sigkill)
            echo "the fixture's runtime did not exit on SIGINT and needed SIGKILL" >&2 ;;
        already-gone)
            echo "the fixture's runtime was gone (exit $FIXTURE_RUNTIME_EXIT_STATUS) before it" \
                "was asked to stop" >&2 ;;
        *)
            echo "the fixture's runtime was never started" >&2 ;;
    esac
    return 1
}

# Whether `graph --stream <stream name>` answers with every node `Running`: a
# stream answers from its load, while its nodes are still `Pending`, and runs
# only once it has started.
the_stream_is_running() {
    "$TATOLAB_EXECUTABLE" graph --stream "$1" 2>/dev/null | "$FIXTURE_PYTHON" -c '
import json, sys
try:
    stream_graph = json.load(sys.stdin)
except ValueError:
    sys.exit(1)
nodes = stream_graph.get("nodes", [])
sys.exit(0 if nodes and all(
    node.get("components", {}).get("state") == "Running" for node in nodes
) else 1)
'
}

# Polls until the stream <stream name> is running, while <loading pid> — the
# `tatolab run` loading it — lives, for up to <seconds> (default 60). Returns 1
# when the pid ends or the budget runs out first.
wait_until_the_stream_is_running() {
    local stream_name="$1" loading_pid="$2" answer_budget_seconds="${3:-60}"
    for _ in $(seq 1 $(( answer_budget_seconds * 2 ))); do
        kill -0 "$loading_pid" 2>/dev/null || return 1
        if the_stream_is_running "$stream_name"; then
            return 0
        fi
        sleep 0.5
    done
    return 1
}

# Prints the name of the one stream the runtime at the local API socket holds,
# read off its machine-wide graph; fails unless it holds exactly one.
name_of_the_sole_stream_the_runtime_holds() {
    "$TATOLAB_EXECUTABLE" graph 2>/dev/null | "$FIXTURE_PYTHON" -c '
import json, sys
try:
    machine_wide_graph = json.load(sys.stdin)
except ValueError:
    sys.exit(1)
streams = machine_wide_graph.get("streams", [])
if len(streams) != 1 or not streams[0].get("stream"):
    sys.exit(1)
print(streams[0]["stream"])
'
}

# Polls until the runtime a Rust rig serves holds exactly one stream and that
# stream is running, while <rig pid> lives, for up to <seconds> (default 30),
# and prints that stream's name. Returns 1 when the pid ends or the budget runs
# out first.
name_of_the_stream_a_rig_serves_once_it_runs() {
    local rig_pid="$1" answer_budget_seconds="${2:-30}" served_stream_name
    for _ in $(seq 1 $(( answer_budget_seconds * 2 ))); do
        kill -0 "$rig_pid" 2>/dev/null || return 1
        if served_stream_name="$(name_of_the_sole_stream_the_runtime_holds)" \
            && the_stream_is_running "$served_stream_name"; then
            printf '%s\n' "$served_stream_name"
            return 0
        fi
        sleep 0.5
    done
    return 1
}

FIXTURE_STREAM_RUN_PIDS=()
FIXTURE_STREAM_RUN_NEEDED_SIGKILL=0

# Loads <stream file:function> of the project at <directory> as <stream name>
# with `tatolab run`, in the background for at most <seconds>, its output in
# <run log>, and returns once the stream is running; 1 when it never runs.
start_the_fixture_stream_run() {
    local project_directory="$1" stream_reference="$2" stream_name="$3"
    local run_log_file="$4" run_budget_seconds="$5"
    timeout --kill-after=5 "$run_budget_seconds" \
        "$TATOLAB_EXECUTABLE" run --dir "$project_directory" \
            --name "$stream_name" "$stream_reference" \
        >"$run_log_file" 2>&1 &
    FIXTURE_STREAM_RUN_PIDS+=("$!")
    wait_until_the_stream_is_running "$stream_name" "$!" 60
}

# SIGTERM to every run `start_the_fixture_stream_run` started, so each graph
# tears down the way a real stop does — `timeout` passes it on, and `tatolab
# run` stops its stream, which the runtime unloads before the run exits. A run
# still alive 10 s later is a hung teardown, not a slow exit: it is killed and
# FIXTURE_STREAM_RUN_NEEDED_SIGKILL set to 1, so the kill never launders it into
# a pass.
stop_the_fixture_stream_runs() {
    local stream_run_pid
    for stream_run_pid in "${FIXTURE_STREAM_RUN_PIDS[@]}"; do
        kill -0 "$stream_run_pid" 2>/dev/null || continue
        kill -TERM "$stream_run_pid" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$stream_run_pid" 2>/dev/null || break
            sleep 0.2
        done
        if kill -0 "$stream_run_pid" 2>/dev/null; then
            # shellcheck disable=SC2034  # read by the driver that sourced this file
            FIXTURE_STREAM_RUN_NEEDED_SIGKILL=1
            kill -9 "$stream_run_pid" 2>/dev/null || true
        fi
        wait "$stream_run_pid" 2>/dev/null || true
    done
    FIXTURE_STREAM_RUN_PIDS=()
}

# Prints how many messages <node>'s output <port> in <stream> has written, off
# `graph`, which counts a port's writes once something reads it. Fails when the
# graph does not answer or names no such node, so a broken read is never a 0.
# The graph renders every node's config, so it is read in a pipe and never kept.
written_messages_of_the_output_port() {
    "$TATOLAB_EXECUTABLE" graph --stream "$1" 2>/dev/null | "$FIXTURE_PYTHON" -c '
import json, sys
stream_graph = json.load(sys.stdin)
node_name, port_name = sys.argv[1], sys.argv[2]
node = next((node for node in stream_graph["nodes"] if node["name"] == node_name), None)
if node is None:
    sys.exit(f"the graph names no node `{node_name}`")
print(node["components"]["metrics"]["written_bags_by_output_port"].get(port_name, 0))
' "$2" "$3"
}

# Concatenates a `tatolab-read-track` dump in its `objects.jsonl` order into
# <elementary stream file> — a codec payload's files are its elementary stream —
# and decodes that with ffmpeg as <format> (`h264` | `hevc`) into
# <PNG directory>/%04d.png, ffmpeg's errors in <ffmpeg log>. Prints the count of
# PNGs decoded.
decode_the_encoded_track_dump_to_pngs() {
    local dump_directory="$1" ffmpeg_elementary_stream_format="$2"
    local elementary_stream_file="$3" png_directory="$4" ffmpeg_log_file="$5"
    "$FIXTURE_PYTHON" -c '
import json, pathlib, sys
dump = pathlib.Path(sys.argv[1])
with open(sys.argv[2], "wb") as elementary_stream:
    for line in (dump / "objects.jsonl").read_text().splitlines():
        elementary_stream.write((dump / json.loads(line)["file"]).read_bytes())
' "$dump_directory" "$elementary_stream_file" || return 1
    mkdir -p "$png_directory"
    ffmpeg -hide_banner -loglevel error -nostdin \
        -f "$ffmpeg_elementary_stream_format" -i "$elementary_stream_file" \
        -fps_mode passthrough -pix_fmt rgb24 \
        "$png_directory/%04d.png" 2>"$ffmpeg_log_file" || return 1
    find "$png_directory" -name '*.png' | wc -l
}
