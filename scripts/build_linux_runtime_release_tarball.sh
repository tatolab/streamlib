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
# bin/tatolabd, bin/tatolab, bin/tatolab-video-poster-frame, bin/uv, lib/tatolab/lend/,
# lib/tatolab/browser-host/, lib/tatolab/imports/, lib/tatolab/core-projects/,
# lib/tatolab/samples/ — plus streamlib-commit, the
# commit it was built from, which names the build install.sh installs it as.
#
# Built inside manylinux_2_28, the glibc floor the engine wheel promises, so
# every binary the tarball carries runs on glibc 2.28 and later;
# `check_linux_runtime_tarball_portability.sh` gates the result at that floor.
# Needs docker. The workspace is mounted, and its target/ written to, as the
# container's root.

set -euo pipefail

workspace_root="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source-path=SCRIPTDIR source=manylinux_2_28_image.sh
. "$workspace_root/scripts/manylinux_2_28_image.sh"

if [ "${1:-}" != "--inside-manylinux" ]; then
	output_directory="${1:?usage: build_linux_runtime_release_tarball.sh <output-directory>}"
	mkdir -p "$output_directory"
	output_directory="$(cd "$output_directory" && pwd)"
	# Read here: inside the container git refuses a workspace another user owns.
	streamlib_commit="$(git -C "$workspace_root" rev-parse HEAD)"
	[ -z "$(git -C "$workspace_root" status --porcelain --untracked-files=no)" ] || {
		echo "the workspace has uncommitted changes: the tarball would not be $streamlib_commit" >&2
		exit 1
	}
	# Each in a container of its own, so the pinned LLVM the driver installs
	# never meets the toolchain this build installs below; inside,
	# build-runtime's staging is then a cache hit.
	bash "$workspace_root/scripts/stage_linux_bundled_vulkan_driver.sh"
	bash "$workspace_root/scripts/stage_slang_compiler_libraries.sh" --fill-the-cache-only
	exec docker run --rm \
		--volume "$workspace_root:/workspace" \
		--volume "$output_directory:/output" \
		--workdir /workspace \
		--env STREAMLIB_RUNNING_IN_MANYLINUX_2_28_IMAGE="$MANYLINUX_2_28_IMAGE" \
		--env STREAMLIB_COMMIT_OF_THE_RUNTIME_UNIT="$streamlib_commit" \
		"$MANYLINUX_2_28_IMAGE" \
		bash /workspace/scripts/build_linux_runtime_release_tarball.sh --inside-manylinux /output
fi

output_directory="$2"

# A bounded, mirror-escaping dnf, for the reason reusable-extension-wheel.yml
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

# libclang for iceoryx2's bindgen; cmake and ninja for aws-lc-sys.
install_build_dependencies_with_bounded_retry clang-devel cmake ninja-build pkgconfig

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
	sh -s -- -y --quiet --profile minimal --default-toolchain stable
# shellcheck source=/dev/null
. "$HOME/.cargo/env"

cargo fetch --locked

uv venv --python 3.12 /tmp/runtime-unit-build-venv
export VIRTUAL_ENV=/tmp/runtime-unit-build-venv
export PATH="$VIRTUAL_ENV/bin:$PATH"

# The browser host BrowserSource opens pages in ships in the unit, built
# against the CEF distribution cef-dll-sys pins.
python3.12 -I scripts/fetch_cef_binary_distribution.py /tmp/cef-binary-distribution
export CEF_PATH=/tmp/cef-binary-distribution
cargo xtask build-runtime --release

runtime_unit_root="$PWD/target/tatolab-runtime"
for browser_host_file in tatolab-browser-host tatolab-browser-subprocess libcef.so LICENSE.txt locales/en-US.pak; do
	[ -f "$runtime_unit_root/lib/tatolab/browser-host/$browser_host_file" ] || {
		echo "the runtime unit has no lib/tatolab/browser-host/$browser_host_file: a release ships the browser host" >&2
		exit 1
	}
done
for first_party_module in tatolab_ops tatolab_testpattern tatolab_compositor; do
	compgen -G "$runtime_unit_root/lib/tatolab/imports/$first_party_module-*.whl" >/dev/null || {
		echo "the runtime unit has no lib/tatolab/imports/$first_party_module-*.whl: a release ships every first-party module" >&2
		exit 1
	}
done

[ -f "$runtime_unit_root/lib/tatolab/core-projects/tatolab-core-nodes/tatolab-project.json" ] || {
	echo "the runtime unit has no lib/tatolab/core-projects/tatolab-core-nodes/: a release ships the Core Nodes project every project imports" >&2
	exit 1
}

for converted_example in examples/*/tatolab-project.json; do
	example_directory="$(dirname "$converted_example")"
	[ -e "$example_directory/Cargo.toml" ] && continue
	sample_name="$(basename "$example_directory")"
	[ -f "$runtime_unit_root/lib/tatolab/samples/$sample_name/tatolab-project.json" ] || {
		echo "the runtime unit has no lib/tatolab/samples/$sample_name/: a release ships every converted example as a sample" >&2
		exit 1
	}
done

release_version="$(cargo metadata --locked --no-deps --format-version 1 |
	python3.12 -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "tatolabd"))')"
tarball_name="tatolab-runtime-$release_version-x86_64-linux.tar.gz"
printf '%s\n' "${STREAMLIB_COMMIT_OF_THE_RUNTIME_UNIT:?}" >"$runtime_unit_root/streamlib-commit"
tar -C "$runtime_unit_root" -czf "$output_directory/$tarball_name" bin lib streamlib-commit
(cd "$output_directory" && sha256sum "$tarball_name" >"$tarball_name.sha256")
echo "built $output_directory/$tarball_name"
