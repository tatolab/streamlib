#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Builds the GLSL compilers build scripts call by name:
#
#   scripts/install_shader_compilers_from_the_locked_shaderc_sources.sh <install-prefix>
#
# writes glslc, glslangValidator and spirv-opt to <install-prefix>/bin/.
# AlmaLinux 8, under manylinux_2_28, packages no glslc, and its glslang and
# SPIRV-Tools are older than rive-runtime's shaders need. Built
# from the shaderc sources vendored in the `shaderc-sys` crate the workspace's
# Cargo.lock pins, fetched from crates.io and checked against the lock's
# checksum, so no cargo, and no lockfile of the caller's own, is needed. Needs
# curl, cmake, ninja, a C++ compiler and a Python 3.11 or later.

set -euo pipefail

install_prefix="${1:?usage: install_shader_compilers_from_the_locked_shaderc_sources.sh <install-prefix>}"
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
curl --proto '=https' --tlsv1.2 -sSfL --retry 5 \
	"https://static.crates.io/crates/shaderc-sys/shaderc-sys-$shaderc_sys_version.crate" \
	-o "$shaderc_sys_crate"
echo "$shaderc_sys_checksum  $shaderc_sys_crate" | sha256sum --check --quiet
tar -xzf "$shaderc_sys_crate" -C "$shaderc_sys_download_directory"

shader_compilers_build_directory="$shaderc_sys_download_directory/glslc-build"
cmake -S "$shaderc_sys_download_directory/shaderc-sys-$shaderc_sys_version/build" \
	-B "$shader_compilers_build_directory" -G Ninja \
	-DCMAKE_BUILD_TYPE=Release \
	-DSHADERC_SKIP_TESTS=ON -DSHADERC_SKIP_EXAMPLES=ON -DSHADERC_SKIP_COPYRIGHT_CHECK=ON \
	-DSPIRV_SKIP_TESTS=ON -DSPIRV_SKIP_EXECUTABLES=OFF \
	-DENABLE_GLSLANG_BINARIES=ON -DSKIP_GLSLANG_INSTALL=ON >/dev/null
ninja -C "$shader_compilers_build_directory" glslc_exe glslang-standalone spirv-opt >/dev/null
mkdir -p "$install_prefix/bin"
cp "$shader_compilers_build_directory/shaderc/glslc/glslc" \
	"$shader_compilers_build_directory/glslang/StandAlone/glslang" \
	"$shader_compilers_build_directory/spirv-tools/tools/spirv-opt" \
	"$install_prefix/bin/"
ln -sf glslang "$install_prefix/bin/glslangValidator"
rm -rf "$shaderc_sys_download_directory"
"$install_prefix/bin/glslc" --version
"$install_prefix/bin/glslangValidator" --version
"$install_prefix/bin/spirv-opt" --version
