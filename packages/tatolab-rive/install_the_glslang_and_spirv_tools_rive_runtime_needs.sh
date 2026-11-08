#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Builds the GLSL tools rive-runtime's build calls by name:
#
#   packages/tatolab-rive/install_the_glslang_and_spirv_tools_rive_runtime_needs.sh <install-prefix>
#
# writes glslangValidator and spirv-opt to <install-prefix>/bin/.
# AlmaLinux 8, under manylinux_2_28, packages a glslang and SPIRV-Tools older
# than rive-runtime's shaders need. Built from the
# shaderc sources the `shaderc-sys` crate vendors, fetched from crates.io and
# checked against the checksum pinned here, so no cargo is needed. Needs curl,
# cmake, ninja and a C++ compiler.

set -euo pipefail

install_prefix="${1:?usage: install_the_glslang_and_spirv_tools_rive_runtime_needs.sh <install-prefix>}"

shaderc_sys_version="0.10.1"
shaderc_sys_checksum="bdceb85b2c6d2c27b95ffe2d341063dfded0aca8046f7f60c544bbeaeaf8bcae"

shaderc_sys_download_directory="$(mktemp -d)"
shaderc_sys_crate="$shaderc_sys_download_directory/shaderc-sys-$shaderc_sys_version.crate"
curl --proto '=https' --tlsv1.2 -sSfL --retry 5 \
	"https://static.crates.io/crates/shaderc-sys/shaderc-sys-$shaderc_sys_version.crate" \
	-o "$shaderc_sys_crate"
echo "$shaderc_sys_checksum  $shaderc_sys_crate" | sha256sum --check --quiet
tar -xzf "$shaderc_sys_crate" -C "$shaderc_sys_download_directory"

shader_compilers_build_directory="$shaderc_sys_download_directory/glslang-build"
cmake -S "$shaderc_sys_download_directory/shaderc-sys-$shaderc_sys_version/build" \
	-B "$shader_compilers_build_directory" -G Ninja \
	-DCMAKE_BUILD_TYPE=Release \
	-DSHADERC_SKIP_TESTS=ON -DSHADERC_SKIP_EXAMPLES=ON -DSHADERC_SKIP_COPYRIGHT_CHECK=ON \
	-DSPIRV_SKIP_TESTS=ON -DSPIRV_SKIP_EXECUTABLES=OFF \
	-DENABLE_GLSLANG_BINARIES=ON -DSKIP_GLSLANG_INSTALL=ON >/dev/null
ninja -C "$shader_compilers_build_directory" glslang-standalone spirv-opt >/dev/null
mkdir -p "$install_prefix/bin"
cp "$shader_compilers_build_directory/glslang/StandAlone/glslang" \
	"$shader_compilers_build_directory/spirv-tools/tools/spirv-opt" \
	"$install_prefix/bin/"
ln -sf glslang "$install_prefix/bin/glslangValidator"
rm -rf "$shaderc_sys_download_directory"
"$install_prefix/bin/glslangValidator" --version
"$install_prefix/bin/spirv-opt" --version
