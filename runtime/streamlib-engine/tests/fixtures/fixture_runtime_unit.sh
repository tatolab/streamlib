# shellcheck shell=bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Sourced by the fixture drivers: the runtime unit a fixture stream runs on,
# the venv its graph is built in, the runtime a fixture starts and stops itself, the
# runtime unit's `tatolab` verbs that load and observe a stream, the stream runs
# a driver starts and stops, the `moq-relay` a stream's gateway nodes send to,
# and `tatolab-read-track`, which reads a track there as any user reads it, with
# the decode of what it wrote.
#
# The runtime unit is what `cargo xtask build-runtime` lays out —
# `bin/tatolab`, `bin/tatolabd` and `lib/tatolab/lend/` — at
# `$STREAMLIB_RUNTIME_UNIT_DIRECTORY`, else `<repository>/target/tatolab-runtime`.
# A driver measures that unit, so rebuild it before a run that should see an
# edit to the engine.
#
# A fixture starts its own `tatolabd`. From a unit built with plain
# `cargo xtask build-runtime`, that runtime takes the machine's real runtime
# lock and serves this user's local API socket, so it refuses to run while
# another runtime holds the machine. From a unit built with
# `--machine-directories-under-a-test-root`, each start gets a scratch
# TATOLAB_TEST_MACHINE_ROOT for its lock, state and local API socket, and its
# loopback HTTP takes the test port below, so it runs beside the machine's
# runtime without touching it. The devices are the machine's
# either way: an exclusive one (a camera, a microphone, a speaker) still takes
# one run at a time. It writes each stream it runs as a project
# of one stream, loads it with `tatolab open --play --dir`, addresses it by the stream
# id the runtime gives it, and removes the project and then stops the runtime.
#
# The fixture venv is `<this directory>/.venv`, which a fixture project's
# `.venv` links to, because the runtime runs a project's Python nodes in its
# `.venv`, and in which the fixture builds each stream's graph. It holds
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

# Whether the runtime unit runs its machine directories under a test root.
the_runtime_unit_runs_under_a_test_machine_root() {
    [ -e "$TEST_MACHINE_ROOT_RUNTIME_UNIT_MARKER" ]
}

# The port a test-root unit's runtime serves its loopback HTTP on: never the
# machine runtime's 4446. Override it when another session's runtime holds it.
FIXTURE_TEST_LOOPBACK_HTTP_PORT="${STREAMLIB_FIXTURE_TEST_LOOPBACK_HTTP_PORT:-14446}"

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
}

# Makes the fixture venv when it is absent, then exits 77 unless it can build
# a fixture stream's graph, and 1 if it carries a runtime of its own.
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

# Builds the microphone and speaker modules from this checkout into the fixture
# venv when it cannot import them, exiting 1 when they do not build. A fixture
# stream that captures or plays imports them, as any stream would.
require_the_audio_modules_in_the_fixture_venv() {
    if "$FIXTURE_PYTHON" -c 'import tatolab.microphone, tatolab.speaker' >/dev/null 2>&1; then
        return 0
    fi
    echo "building tatolab-microphone and tatolab-speaker into $FIXTURE_VENV_DIRECTORY" >&2
    if ! VIRTUAL_ENV="$FIXTURE_VENV_DIRECTORY" uv pip install --quiet \
        "$FIXTURE_REPOSITORY_ROOT/packages/tatolab-microphone" \
        "$FIXTURE_REPOSITORY_ROOT/packages/tatolab-speaker" >&2; then
        echo "ERROR: the audio modules did not build into $FIXTURE_VENV_DIRECTORY" >&2
        exit 1
    fi
}

# Builds the H.264, H.265, test-pattern and MoQ gateway modules from this
# checkout into the fixture venv when it cannot import them, exiting 1 when they
# do not build. A fixture stream that encodes, decodes or sends video imports
# them, as any stream would.
require_the_codec_modules_in_the_fixture_venv() {
    if "$FIXTURE_PYTHON" -c 'import tatolab.h264, tatolab.h265, tatolab.testpattern, tatolab.moq' >/dev/null 2>&1; then
        return 0
    fi
    echo "building tatolab-h264, tatolab-h265, tatolab-testpattern and tatolab-moq into $FIXTURE_VENV_DIRECTORY" >&2
    if ! VIRTUAL_ENV="$FIXTURE_VENV_DIRECTORY" uv pip install --quiet \
        "$FIXTURE_REPOSITORY_ROOT/packages/streamlib-h264" \
        "$FIXTURE_REPOSITORY_ROOT/packages/streamlib-h265" \
        "$FIXTURE_REPOSITORY_ROOT/packages/tatolab-moq" \
        "$FIXTURE_REPOSITORY_ROOT/packages/tatolab-testpattern" >&2; then
        echo "ERROR: the codec modules did not build into $FIXTURE_VENV_DIRECTORY" >&2
        exit 1
    fi
}

# Runs a verb of the runtime unit's `tatolab` (`graph`, `logs`, `streams`).
tatolab_observation_verb() {
    "$TATOLAB_EXECUTABLE" "$@"
}

# The relay a fixture stream's `MoqGateway` dials and `tatolab-read-track`
# reads at: a `moq-relay` of the version `packages/tatolab-moq` tests against,
# built into the workspace's `target/`, which the fixture starts on loopback,
# open to anonymous publish and subscribe, its QUIC and its certificate's
# fingerprint on one port. Override the port when another session's relay
# holds it.
FIXTURE_RELAY_CRATE_VERSION="0.17.7"
FIXTURE_RELAY_INSTALL_ROOT="$FIXTURE_REPOSITORY_ROOT/target/fixture-relay"
FIXTURE_RELAY_EXECUTABLE="$FIXTURE_RELAY_INSTALL_ROOT/bin/moq-relay"
FIXTURE_RELAY_PORT="${STREAMLIB_FIXTURE_RELAY_PORT:-14443}"
FIXTURE_RELAY_URL="http://127.0.0.1:$FIXTURE_RELAY_PORT"
FIXTURE_RELAY_PID=""

# Builds the fixture relay into its install root when it is absent, exiting 1
# when it does not build. `cargo install` leaves a second run of the same
# version alone.
require_the_fixture_relay() {
    [ -x "$FIXTURE_RELAY_EXECUTABLE" ] && return 0
    echo "building moq-relay $FIXTURE_RELAY_CRATE_VERSION into $FIXTURE_RELAY_INSTALL_ROOT" >&2
    if ! cargo install moq-relay --version "=$FIXTURE_RELAY_CRATE_VERSION" --locked \
        --no-default-features --features noq --root "$FIXTURE_RELAY_INSTALL_ROOT" >&2; then
        echo "ERROR: moq-relay $FIXTURE_RELAY_CRATE_VERSION does not build" >&2
        exit 1
    fi
}

# Starts the fixture relay in the background as FIXTURE_RELAY_PID, its
# configuration in <directory> and its output in <log file>, and returns once
# it serves its certificate's fingerprint; 1, with the log's tail, otherwise.
start_the_fixture_relay() {
    local relay_log_file="$1" relay_configuration_directory="$2"
    mkdir -p "$relay_configuration_directory"
    cat >"$relay_configuration_directory/moq-relay.toml" <<TOML
drain_timeout = "0s"

[listen]
bind = "127.0.0.1:$FIXTURE_RELAY_PORT"

[listen.tls]
generate = ["localhost"]

[web.http]
listen = "127.0.0.1:$FIXTURE_RELAY_PORT"

# The relay's own dial socket, on IPv4 too, for a host with no IPv6.
[connect]
bind = "127.0.0.1:0"

[auth]
public = ["**"]
TOML
    "$FIXTURE_RELAY_EXECUTABLE" "$relay_configuration_directory/moq-relay.toml" \
        >"$relay_log_file" 2>&1 &
    FIXTURE_RELAY_PID=$!
    for _ in $(seq 1 120); do
        if ! kill -0 "$FIXTURE_RELAY_PID" 2>/dev/null; then
            FIXTURE_RELAY_PID=""
            echo "REFUSED: the fixture relay exited before it served on port $FIXTURE_RELAY_PORT:" >&2
            tail -20 "$relay_log_file" >&2
            return 1
        fi
        if "$FIXTURE_PYTHON" -c '
import sys, urllib.request
urllib.request.urlopen(sys.argv[1] + "/certificate.sha256", timeout=1).read()
' "$FIXTURE_RELAY_URL" >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.25
    done
    echo "REFUSED: the fixture relay did not serve on port $FIXTURE_RELAY_PORT within 30 s:" >&2
    tail -20 "$relay_log_file" >&2
    stop_the_fixture_relay
    return 1
}

# SIGINT to the fixture relay, then SIGKILL after 5 s.
stop_the_fixture_relay() {
    [ -n "$FIXTURE_RELAY_PID" ] || return 0
    kill -INT "$FIXTURE_RELAY_PID" 2>/dev/null || true
    for _ in $(seq 1 25); do
        kill -0 "$FIXTURE_RELAY_PID" 2>/dev/null || break
        sleep 0.2
    done
    kill -KILL "$FIXTURE_RELAY_PID" 2>/dev/null || true
    wait "$FIXTURE_RELAY_PID" 2>/dev/null || true
    FIXTURE_RELAY_PID=""
}

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

# Reads <count> objects of <track> in <broadcast> at the fixture relay into
# <directory>, as `tatolab-read-track` writes them.
read_the_track_at_the_fixture_relay() {
    local broadcast="$1" track="$2" object_count="$3" dump_directory="$4"
    "$TATOLAB_READ_TRACK_EXECUTABLE" "$FIXTURE_RELAY_URL" "$broadcast" "$track" \
        --count "$object_count" --out "$dump_directory"
}

# Whether a runtime answers at the local API socket the fixture's `tatolab`
# reaches now: this user's, or the current test machine root's.
a_runtime_answers_at_the_local_api_socket() {
    "$TATOLAB_EXECUTABLE" streams >/dev/null 2>&1
}

# Returns 1, saying so, when a runtime already answers at this user's local
# API socket: a Rust rig serves that socket itself, and refuses it held. A
# test-root unit's runtime serves a socket of its own, so it never refuses.
refuse_while_a_runtime_answers_at_the_local_api_socket() {
    if the_runtime_unit_runs_under_a_test_machine_root; then
        return 0
    fi
    if a_runtime_answers_at_the_local_api_socket; then
        echo "REFUSED: a runtime already answers at this user's local API socket; stop it" \
            "(Ctrl-C in its terminal) and run the fixture again — a fixture starts its own:" >&2
        "$TATOLAB_EXECUTABLE" streams >&2 || true
        return 1
    fi
}

FIXTURE_RUNTIME_PID=""
# The scratch TATOLAB_TEST_MACHINE_ROOT of the running fixture runtime, when
# the unit is a test-root unit; exported as TATOLAB_TEST_MACHINE_ROOT while it
# runs, so every `tatolab` verb reaches it.
FIXTURE_TEST_MACHINE_ROOT=""
# How the fixture's runtime ended: never-started, stopped-cleanly,
# needed-sigkill or already-gone, with its exit status beside it.
FIXTURE_RUNTIME_STOP_OUTCOME="never-started"
FIXTURE_RUNTIME_EXIT_STATUS=""

# Starts the runtime unit's `tatolabd` in the background as
# FIXTURE_RUNTIME_PID, its output — the engine's log mirror, every stream's
# records among it — in <log file>, and returns once its local API answers.
# Every setting a fixture node reads from the environment while it runs goes
# here: the runtime starts its processor interpreters with its own environment,
# never `tatolab open --play`'s. A setting its graph generator reads goes to
# `write_a_one_stream_fixture_project` instead.
#
# Its state directory sits under <state home>: XDG_STATE_HOME names it on
# Linux, and HOME on macOS, where the state directory is `$HOME/Library/
# Application Support/Tatolab`. So it re-loads none of this user's recorded streams
# and leaves its own log beside the fixture's evidence. From a plain unit, the
# runtime directory and the lock read neither variable, so the local API socket
# stays where `tatolab` looks, and it takes the machine's real runtime lock:
# when another runtime holds the machine, `tatolabd` refuses naming the holder,
# and this returns 1 having printed that refusal. From a test-root unit, it
# runs under a fresh FIXTURE_TEST_MACHINE_ROOT, which the stop removes.
start_the_fixture_runtime() {
    local runtime_log_file="$1" runtime_state_home="$2"
    local a_runtime_answered_before_the_start=0 runtime_exit_status
    local -a state_directory_environment=("XDG_STATE_HOME=$runtime_state_home")
    if [ "$(uname -s)" = Darwin ]; then
        state_directory_environment+=("HOME=$runtime_state_home")
    fi
    if the_runtime_unit_runs_under_a_test_machine_root; then
        # Short and under /tmp: the local API socket's path must stay under
        # 104 bytes.
        FIXTURE_TEST_MACHINE_ROOT="$(mktemp -d /tmp/tl-fx-XXXXXX)"
        export TATOLAB_TEST_MACHINE_ROOT="$FIXTURE_TEST_MACHINE_ROOT"
        state_directory_environment+=(
            "TATOLAB_TEST_LOOPBACK_HTTP_PORT=$FIXTURE_TEST_LOOPBACK_HTTP_PORT"
        )
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
            remove_the_fixture_test_machine_root
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
    if [ -z "$FIXTURE_RUNTIME_PID" ]; then
        remove_the_fixture_test_machine_root
        return 0
    fi
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
    remove_the_fixture_test_machine_root
}

# Removes the scratch test machine root of a fixture runtime that has exited,
# and stops exporting it.
remove_the_fixture_test_machine_root() {
    [ -n "$FIXTURE_TEST_MACHINE_ROOT" ] || return 0
    rm -rf "$FIXTURE_TEST_MACHINE_ROOT"
    FIXTURE_TEST_MACHINE_ROOT=""
    unset TATOLAB_TEST_MACHINE_ROOT
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

# Whether `graph --stream <stream id>` answers with every node `Running`: a
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

# Polls until the stream <stream id> is running, for up to <seconds> (default
# 60). Returns 1 when the budget runs out first.
wait_until_the_stream_is_running() {
    local stream_id="$1" answer_budget_seconds="${2:-60}"
    for _ in $(seq 1 $(( answer_budget_seconds * 2 ))); do
        if the_stream_is_running "$stream_id"; then
            return 0
        fi
        sleep 0.5
    done
    return 1
}

# Whether the runtime holds <stream id> loaded: its machine-wide graph names it.
the_stream_is_loaded() {
    "$TATOLAB_EXECUTABLE" graph 2>/dev/null | "$FIXTURE_PYTHON" -c '
import json, sys
try:
    machine_wide_graph = json.load(sys.stdin)
except ValueError:
    sys.exit(1)
sys.exit(0 if any(
    loaded_stream.get("stream") == sys.argv[1]
    for loaded_stream in machine_wide_graph.get("streams", [])
) else 1)
' "$1"
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

# Writes at <project directory> a project of one stream, the way a user's
# project is laid out: a `tatolab-project.json` naming the project after its
# folder; `streams/<stream name>.graph.json`, the graph the `@graph_generator`
# <graph function> of <module file> builds, run in <venv directory>'s Python
# and in the caller's environment, so a setting the generator reads from the
# environment must be in it at this call; every module beside <module file>
# linked in, so a node type the graph names resolves in its processor
# interpreter; a `pyproject.toml`; and `.venv` linked to <venv directory>, the
# venv the runtime runs the project's Python nodes in. Written owner-only: a
# generator may put a credential from the environment into a node's config.
write_a_one_stream_fixture_project() {
    local project_directory="$1" module_file="$2" graph_function="$3"
    local stream_name="$4" venv_directory="$5"
    local module_directory module_name module_beside
    module_directory="$(cd "$(dirname "$module_file")" && pwd)"
    module_name="$(basename "$module_file" .py)"
    mkdir -p "$project_directory"
    chmod 700 "$project_directory"
    for module_beside in "$module_directory"/*.py; do
        ln -sfn "$module_beside" "$project_directory/$(basename "$module_beside")"
    done
    ln -sfn "$venv_directory" "$project_directory/.venv"
    cat >"$project_directory/pyproject.toml" <<TOML
[project]
name = "$(basename "$project_directory")"
version = "0.0.0"
requires-python = ">=3.12"
TOML
    "$venv_directory/bin/python" -I -c '
import importlib, json, pathlib, sys, uuid
from tatolab.stream._stream_graph_builder import run_graph_generator

module_directory, module_name, graph_function_name, project_directory, stream_name = sys.argv[1:]
project_directory = pathlib.Path(project_directory)
sys.path.insert(0, module_directory)
graph = run_graph_generator(getattr(importlib.import_module(module_name), graph_function_name))

def nodes_holding_a_uuid_each(level, asset_namespace, path_of_names):
    nodes = level.get("nodes")
    if not isinstance(nodes, list):
        return level
    nodes_with_a_uuid = []
    for node in nodes:
        node_path = path_of_names + str(node.get("name", ""))
        node = node if "id" in node else {"id": str(uuid.uuid5(asset_namespace, node_path)), **node}
        if isinstance(node.get("fragment"), dict):
            node = {**node, "fragment": nodes_holding_a_uuid_each(
                node["fragment"], asset_namespace, node_path + "/")}
        nodes_with_a_uuid.append(node)
    return {**level, "nodes": nodes_with_a_uuid}

stream_asset = uuid.uuid4()
project_file = {"project": str(uuid.uuid4()), "name": project_directory.name, "dependencies": []}
(project_directory / "tatolab-project.json").write_text(json.dumps(project_file, indent=2))
graph_documents_directory = project_directory / "streams"
graph_documents_directory.mkdir(exist_ok=True)
graph_document = {
    "asset": str(stream_asset),
    **nodes_holding_a_uuid_each(graph, stream_asset, ""),
    "stream": stream_name,
}
(graph_documents_directory / (stream_name + ".graph.json")).write_text(
    json.dumps(graph_document, indent=2, allow_nan=False))
' "$module_directory" "$module_name" "$graph_function" "$project_directory" "$stream_name"
}

# Prints the stream asset UUID the one graph document of the project at
# <project directory> holds.
stream_asset_of_the_fixture_project() {
    "$FIXTURE_PYTHON" -c '
import json, pathlib, sys
graph_documents = list((pathlib.Path(sys.argv[1]) / "streams").glob("*.graph.json"))
if len(graph_documents) != 1:
    sys.exit(f"{sys.argv[1]} holds {len(graph_documents)} graph documents, not one")
print(json.loads(graph_documents[0].read_text())["asset"])
' "$1"
}

# Prints the stream id the runtime gave the stream asset <stream asset>, read
# off the ID and ASSET columns of `tatolab streams`; fails when it lists none.
stream_id_the_runtime_gave_the_stream_asset() {
    "$TATOLAB_EXECUTABLE" streams 2>/dev/null \
        | awk -v stream_asset="$1" '$NF == stream_asset { print $1; found = 1; exit } END { exit !found }'
}

FIXTURE_PROJECT_DIRECTORIES=()
FIXTURE_PROJECT_STREAM_IDS=()
FIXTURE_PROJECT_REMOVAL_TIMED_OUT=0

# Queues a load of the project at <project directory> with `tatolab open --play --dir`,
# its output in <run log>, and returns once the runtime lists its stream and
# that stream runs, the stream id the runtime gave it in FIXTURE_PROJECT_STREAM_ID;
# 1 when the load is refused or the stream has not run within 60 s, with the
# runtime's stream listing and the project's log appended to <run log>.
start_the_fixture_project() {
    local project_directory="$1" run_log_file="$2" stream_asset
    FIXTURE_PROJECT_STREAM_ID=""
    FIXTURE_PROJECT_DIRECTORIES+=("$project_directory")
    if stream_asset="$(stream_asset_of_the_fixture_project "$project_directory")" \
        && "$TATOLAB_EXECUTABLE" start --dir "$project_directory" >"$run_log_file" 2>&1; then
        for _ in $(seq 1 120); do
            FIXTURE_PROJECT_STREAM_ID="$(stream_id_the_runtime_gave_the_stream_asset "$stream_asset")" \
                && break
            sleep 0.5
        done
        if [ -n "$FIXTURE_PROJECT_STREAM_ID" ]; then
            FIXTURE_PROJECT_STREAM_IDS+=("$FIXTURE_PROJECT_STREAM_ID")
            wait_until_the_stream_is_running "$FIXTURE_PROJECT_STREAM_ID" 60 && return 0
        fi
    fi
    {
        "$TATOLAB_EXECUTABLE" streams
        "$TATOLAB_EXECUTABLE" logs --dir "$project_directory"
    } >>"$run_log_file" 2>&1
    return 1
}

# Queues `tatolab rm --dir` of every project `start_the_fixture_project`
# loaded, so each graph tears down the way a user's removal tears it down, and
# waits up to 10 s for each stream to unload. A stream still loaded then is a
# hung teardown, not a slow unload: FIXTURE_PROJECT_REMOVAL_TIMED_OUT is set to
# 1, so the runtime's stop taking it down never launders it into a pass.
remove_the_fixture_projects() {
    local project_directory stream_id
    for project_directory in "${FIXTURE_PROJECT_DIRECTORIES[@]}"; do
        "$TATOLAB_EXECUTABLE" rm --dir "$project_directory" >/dev/null 2>&1 || true
    done
    for stream_id in "${FIXTURE_PROJECT_STREAM_IDS[@]}"; do
        for _ in $(seq 1 50); do
            the_stream_is_loaded "$stream_id" || continue 2
            sleep 0.2
        done
        # shellcheck disable=SC2034  # read by the driver that sourced this file
        FIXTURE_PROJECT_REMOVAL_TIMED_OUT=1
    done
    FIXTURE_PROJECT_DIRECTORIES=()
    FIXTURE_PROJECT_STREAM_IDS=()
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
