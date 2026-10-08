# shellcheck shell=bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Sourced by the fixture drivers: the runtime unit a fixture stream runs on,
# the venv it compiles in, and the observation verbs reached through the lend.
#
# The runtime unit is what `cargo xtask build-runtime` lays out —
# `bin/tatolab`, `bin/tatolabd` and `lib/tatolab/lend/` — at
# `$STREAMLIB_RUNTIME_UNIT_DIRECTORY`, else `<repository>/target/tatolab-runtime`.
# A driver measures that unit, so rebuild it before a run that should see an
# edit to the engine.
#
# The fixture venv is `<this directory>/.venv`, because `tatolab run --dir`
# compiles a stream in its anchor's `.venv`. It holds `tatolab-stream` from this
# checkout, editable, and numpy, which the fixture nodes import; never the
# runtime, which a processor interpreter borrows from the lend. It is made with
# uv the first time a driver needs it; delete the directory to have it made
# again. To use a venv prepared elsewhere instead, name it with
# STREAMLIB_FIXTURE_VENV: it is checked the same way and `<this directory>/.venv`
# becomes a symlink to it, which stays in use until deleted.

FIXTURE_DIRECTORY="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
FIXTURE_REPOSITORY_ROOT="$(cd "$FIXTURE_DIRECTORY/../../../.." && pwd)"
RUNTIME_UNIT_DIRECTORY="${STREAMLIB_RUNTIME_UNIT_DIRECTORY:-$FIXTURE_REPOSITORY_ROOT/target/tatolab-runtime}"
TATOLAB_EXECUTABLE="$RUNTIME_UNIT_DIRECTORY/bin/tatolab"
TATOLABD_EXECUTABLE="$RUNTIME_UNIT_DIRECTORY/bin/tatolabd"
RUNTIME_UNIT_LEND_DIRECTORY="$RUNTIME_UNIT_DIRECTORY/lib/tatolab/lend"
FIXTURE_VENV_DIRECTORY="$FIXTURE_DIRECTORY/.venv"
FIXTURE_PYTHON="$FIXTURE_VENV_DIRECTORY/bin/python"

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

# Runs a verb of the Python `streamlib` CLI (`nodes`, `graph`, `tap`, `logs`,
# `exchange`) from the lend, until the native CLI carries them.
streamlib_observation_verb() {
    PYTHONPATH="$RUNTIME_UNIT_LEND_DIRECTORY" "$FIXTURE_PYTHON" -m tatolab.runtime.cli "$@"
}

# Runs the fixture venv's interpreter with the lend importable, for a fixture
# script that reads `tatolab.runtime` internals.
python_with_the_lend() {
    PYTHONPATH="$RUNTIME_UNIT_LEND_DIRECTORY" "$FIXTURE_PYTHON" "$@"
}

# Prints the runtime_id of the live node a launched process runs, and fails
# until exactly one has registered. The launched pid is `tatolab run`, or a
# wrapper (`timeout`) around it; the node is the `tatolabd` beneath.
runtime_id_of_the_node_launched_as() {
    python_with_the_lend "$FIXTURE_DIRECTORY/runtime_id_of_launched_node.py" "$1"
}
