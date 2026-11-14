#!/usr/bin/env bash
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
#
# Runs the portability gate over a built Linux runtime tarball at the glibc floor
# the engine wheel promises:
#
#   scripts/check_linux_runtime_tarball_portability.sh <linux-runtime-tarball>
#
# The tarball is unpacked inside the manylinux_2_28 image it was built in, and
# `tests_against_the_runtime_unit_lend/` runs against its lend from a venv that
# holds this checkout's `tatolab-stream` and nothing of the runtime. Needs docker.

set -euo pipefail

RUNTIME_UNIT_GLIBC_FLOOR="2.28"

workspace_root="$(cd "$(dirname "$0")/.." && pwd)"
script_name="$(basename "$0")"
# shellcheck source-path=SCRIPTDIR source=manylinux_2_28_image.sh
. "$workspace_root/scripts/manylinux_2_28_image.sh"

if [ "${1:-}" != "--inside-manylinux" ]; then
	runtime_tarball="${1:?usage: $script_name <linux-runtime-tarball>}"
	runtime_tarball="$(cd "$(dirname "$runtime_tarball")" && pwd)/$(basename "$runtime_tarball")"
	exec docker run --rm \
		--volume "$runtime_tarball:/linux-runtime-tarball.tar.gz:ro" \
		--volume "$workspace_root:/workspace:ro" \
		--env STREAMLIB_RUNNING_IN_MANYLINUX_2_28_IMAGE="$MANYLINUX_2_28_IMAGE" \
		"$MANYLINUX_2_28_IMAGE" \
		bash "/workspace/scripts/$script_name" --inside-manylinux
fi

# The gate finds the unit's `bin/` from the lend's path, which must end in
# `tatolab-runtime/lib/tatolab/lend`.
runtime_unit_root=/tmp/unpacked/tatolab-runtime
mkdir -p "$runtime_unit_root"
tar -xzf /linux-runtime-tarball.tar.gz -C "$runtime_unit_root"

python_3_12=/opt/python/cp312-cp312/bin/python3
# Copied with its LICENSE symlink resolved: the workspace is mounted read-only.
cp -rL /workspace/sdk/tatolab-stream /tmp/tatolab-stream-source
mkdir -p /tmp/streamlib-python-wheel
cp -L /workspace/sdk/streamlib-python-wheel/pyproject.toml /tmp/streamlib-python-wheel/
cp -rL /workspace/sdk/streamlib-python-wheel/tests_against_the_runtime_unit_lend /tmp/streamlib-python-wheel/
"$python_3_12" -m venv /tmp/runtime-unit-gate-venv
/tmp/runtime-unit-gate-venv/bin/pip install --quiet /tmp/tatolab-stream-source pytest

cd /tmp/streamlib-python-wheel
PYTHONPATH="$runtime_unit_root/lib/tatolab/lend" \
	TATOLAB_RUNTIME_UNIT_GLIBC_FLOOR="$RUNTIME_UNIT_GLIBC_FLOOR" \
	/tmp/runtime-unit-gate-venv/bin/python -m pytest -v tests_against_the_runtime_unit_lend/
