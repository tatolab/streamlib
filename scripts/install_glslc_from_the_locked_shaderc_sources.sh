#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Builds glslc and installs it on PATH's behalf:
#
#   scripts/install_glslc_from_the_locked_shaderc_sources.sh <install-prefix>
#
# writes <install-prefix>/bin/glslc. AlmaLinux 8, under manylinux_2_28, packages
# no glslc, and every build script that compiles GLSL calls it by name. Built
# from the shaderc sources vendored in the `shaderc-sys` crate the workspace's
# Cargo.lock pins, fetched from crates.io and checked against the lock's
# checksum, so no cargo, and no lockfile of the caller's own, is needed. Needs
# curl, cmake, ninja, a C++ compiler and a Python 3.11 or later.

set -euo pipefail

install_prefix="${1:?usage: install_glslc_from_the_locked_shaderc_sources.sh <install-prefix>}"
workspace_root="$(cd "$(dirname "$0")/.." && pwd)"

python_with_tomllib="$(command -v python3.12 || command -v python3)"
read -r shaderc_sys_version shaderc_sys_checksum < <("$python_with_tomllib" - "$workspace_root/Cargo.lock" <<'EOF'
import sys
import tomllib

with open(sys.argv[1], "rb") as cargo_lock:
    shaderc_sys = next(package for package in tomllib.load(cargo_lock)["package"] if package["name"] == "shaderc-sys")
print(shaderc_sys["version"], shaderc_sys["checksum"])
EOF
)

shaderc_sys_download_directory="$(mktemp -d)"
shaderc_sys_crate="$shaderc_sys_download_directory/shaderc-sys-$shaderc_sys_version.crate"
curl --proto '=https' --tlsv1.2 -sSfL --retry 5 --retry-all-errors \
	"https://static.crates.io/crates/shaderc-sys/shaderc-sys-$shaderc_sys_version.crate" \
	-o "$shaderc_sys_crate"
echo "$shaderc_sys_checksum  $shaderc_sys_crate" | sha256sum --check --quiet
tar -xzf "$shaderc_sys_crate" -C "$shaderc_sys_download_directory"

glslc_build_directory="$shaderc_sys_download_directory/glslc-build"
cmake -S "$shaderc_sys_download_directory/shaderc-sys-$shaderc_sys_version/build" \
	-B "$glslc_build_directory" -G Ninja \
	-DCMAKE_BUILD_TYPE=Release \
	-DSHADERC_SKIP_TESTS=ON -DSHADERC_SKIP_EXAMPLES=ON -DSHADERC_SKIP_COPYRIGHT_CHECK=ON \
	-DSPIRV_SKIP_TESTS=ON -DSPIRV_SKIP_EXECUTABLES=ON \
	-DENABLE_GLSLANG_BINARIES=OFF -DSKIP_GLSLANG_INSTALL=ON >/dev/null
ninja -C "$glslc_build_directory" glslc_exe >/dev/null
mkdir -p "$install_prefix/bin"
cp "$glslc_build_directory/shaderc/glslc/glslc" "$install_prefix/bin/glslc"
rm -rf "$shaderc_sys_download_directory"
"$install_prefix/bin/glslc" --version
