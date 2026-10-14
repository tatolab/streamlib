#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Builds the release's Linux runtime tarball:
#
#   scripts/build_linux_runtime_release_tarball.sh <output-directory>
#
# writes tatolab-runtime-<version>-x86_64-linux.tar.gz and its .sha256 there.
# The tarball is `cargo xtask build-runtime --release`'s install prefix —
# bin/tatolabd, bin/tatolab, lib/tatolab/lend/ — plus the systemd user units in
# lib/systemd/user/, where install.sh takes them from.
#
# Built inside manylinux_2_28, the glibc floor the engine wheel promises, so
# every binary the tarball carries runs on glibc 2.28 and later, and the
# portability gate runs over the result at that floor. Needs docker. The
# workspace is mounted, and its target/ written to, as the container's root.

set -euo pipefail

RUNTIME_UNIT_GLIBC_FLOOR="2.28"

workspace_root="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source-path=SCRIPTDIR source=manylinux_2_28_image.sh
. "$workspace_root/scripts/manylinux_2_28_image.sh"

if [ "${1:-}" != "--inside-manylinux" ]; then
	output_directory="${1:?usage: build_linux_runtime_release_tarball.sh <output-directory>}"
	mkdir -p "$output_directory"
	output_directory="$(cd "$output_directory" && pwd)"
	# In a container of its own, so the pinned LLVM it installs never meets the
	# toolchain this build installs below; inside, build-runtime's staging is
	# then a cache hit.
	bash "$workspace_root/scripts/stage_linux_bundled_vulkan_driver.sh"
	exec docker run --rm \
		--volume "$workspace_root:/workspace" \
		--volume "$output_directory:/output" \
		--workdir /workspace \
		--env STREAMLIB_RUNNING_IN_MANYLINUX_2_28_IMAGE="$MANYLINUX_2_28_IMAGE" \
		"$MANYLINUX_2_28_IMAGE" \
		bash /workspace/scripts/build_linux_runtime_release_tarball.sh --inside-manylinux /output
fi

output_directory="$2"

# A bounded, mirror-escaping dnf, for the reason release-extension-wheel.yml
# gives: a slow AlmaLinux mirror is only detected by a wall-clock bound, and
# `dnf clean all` makes the next attempt re-resolve the mirrorlist.
install_build_dependencies_with_bounded_retry() {
	for attempt in 1 2 3; do
		if timeout --kill-after=10s 420s \
			dnf install -y --setopt=retries=3 --setopt=timeout=30 "$@"; then
			return 0
		fi
		echo "dnf attempt $attempt failed or blew its 420 s bound; re-resolving the mirrorlist" >&2
		dnf clean all >/dev/null 2>&1 || true
	done
	echo "dnf install did not complete in three bounded attempts" >&2
	return 1
}

# libclang for iceoryx2's bindgen; cmake and ninja for the vendored shaderc.
install_build_dependencies_with_bounded_retry clang-devel cmake ninja-build pkgconfig

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
	sh -s -- -y --quiet --profile minimal --default-toolchain stable
# shellcheck source=/dev/null
. "$HOME/.cargo/env"

cargo fetch --locked

# The engine's build script compiles its shaders with glslc, which AlmaLinux 8
# does not package. Built from the shaderc sources Cargo.lock already pins for
# the engine's own compiler.
shaderc_sys_manifest_path="$(cargo metadata --locked --format-version 1 |
	python3.12 -c 'import json, sys; print(next(p["manifest_path"] for p in json.load(sys.stdin)["packages"] if p["name"] == "shaderc-sys"))')"
shaderc_source_directory="$(dirname "$shaderc_sys_manifest_path")/build"
glslc_build_directory="$(mktemp -d)"
cmake -S "$shaderc_source_directory" -B "$glslc_build_directory" -G Ninja \
	-DCMAKE_BUILD_TYPE=Release \
	-DSHADERC_SKIP_TESTS=ON -DSHADERC_SKIP_EXAMPLES=ON -DSHADERC_SKIP_COPYRIGHT_CHECK=ON \
	-DSPIRV_SKIP_TESTS=ON -DSPIRV_SKIP_EXECUTABLES=ON \
	-DENABLE_GLSLANG_BINARIES=OFF -DSKIP_GLSLANG_INSTALL=ON >/dev/null
ninja -C "$glslc_build_directory" glslc_exe >/dev/null
mkdir -p /opt/glslc/bin
cp "$glslc_build_directory/shaderc/glslc/glslc" /opt/glslc/bin/glslc
export PATH="/opt/glslc/bin:$PATH"
glslc --version

uv venv --python 3.12 /tmp/runtime-unit-build-venv
export VIRTUAL_ENV=/tmp/runtime-unit-build-venv
cargo xtask build-runtime --release

runtime_unit_root="$PWD/target/tatolab-runtime"
mkdir -p "$runtime_unit_root/lib/systemd/user"
cp installer/tatolabd.service installer/tatolabd-restart-for-a-display-at-login.service \
	"$runtime_unit_root/lib/systemd/user/"

uv build --wheel --out-dir /tmp/tatolab-stream-dist sdk/tatolab-stream
uv venv --python 3.12 /tmp/runtime-unit-gate-venv
VIRTUAL_ENV=/tmp/runtime-unit-gate-venv uv pip install /tmp/tatolab-stream-dist/*.whl pytest
(
	cd sdk/streamlib-python-wheel
	PYTHONPATH="$runtime_unit_root/lib/tatolab/lend" \
		TATOLAB_RUNTIME_UNIT_GLIBC_FLOOR="$RUNTIME_UNIT_GLIBC_FLOOR" \
		/tmp/runtime-unit-gate-venv/bin/python -m pytest -v tests_against_the_runtime_unit_lend/
)

release_version="$(cargo metadata --locked --no-deps --format-version 1 |
	python3.12 -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "tatolabd"))')"
tarball_name="tatolab-runtime-$release_version-x86_64-linux.tar.gz"
tar -C "$runtime_unit_root" -czf "$output_directory/$tarball_name" bin lib
(cd "$output_directory" && sha256sum "$tarball_name" >"$tarball_name.sha256")
echo "built $output_directory/$tarball_name"
