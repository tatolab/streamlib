#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Stages the Vulkan loader and MoltenVK into the macOS wheel's package tree, so
# `maturin build` ships them and a stock Apple Silicon Mac needs no Vulkan SDK.
#
# Writes `libvulkan.1.dylib`, `libMoltenVK.dylib` and `MoltenVK_icd.json` into
# `sdk/streamlib-python-wheel/python/streamlib/_vulkan_driver/` (or the
# directory given as $1). The engine dlopens the loader there by absolute path,
# and `streamlib/__init__.py` points the loader at the ICD manifest beside it.
#
# Every binary this writes ends in `codesign -f -s -`. Any post-link rewrite —
# `lipo -thin` here — invalidates the signature, and an arm64 dylib with a
# broken signature loads on the machine that built it and fails on every other.

set -euo pipefail

# 1.4.1 is the floor: camera zero-copy imports IOSurface memory as a storage
# buffer through `VK_EXT_external_memory_host`, which 1.4.0 refuses in its
# spec-correct form.
MOLTENVK_RELEASE_TAG="v1.4.2"
MOLTENVK_MACOS_TARBALL_SHA256="f95765a6229cb7b915990a2890ce12ebe36a730b021545d3d52ae69ce4c4024e"
VULKAN_LOADER_TAG="vulkan-sdk-1.4.357.0"

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
wheel_project_root="$workspace_root/sdk/streamlib-python-wheel"
bundled_vulkan_driver_directory="${1:-$wheel_project_root/python/streamlib/_vulkan_driver}"

# The wheel's own deployment target, read from the one place it is pinned so the
# loader cannot be built for a newer macOS than the wheel's tag claims.
macos_deployment_target="$(
  sed -n '/^\[tool\.maturin\.target\.aarch64-apple-darwin\]/,/^\[/s/^macos-deployment-target = "\(.*\)"/\1/p' \
    "$wheel_project_root/pyproject.toml"
)"
if [ -z "$macos_deployment_target" ]; then
  echo "error: no macos-deployment-target under [tool.maturin.target.aarch64-apple-darwin] in $wheel_project_root/pyproject.toml" >&2
  exit 1
fi

scratch_directory="$(mktemp -d)"
trap 'rm -rf "$scratch_directory"' EXIT

rm -rf "$bundled_vulkan_driver_directory"
mkdir -p "$bundled_vulkan_driver_directory"

echo "MoltenVK ${MOLTENVK_RELEASE_TAG}: the Khronos release build, thinned to arm64"
curl --fail --silent --show-error --location --retry 4 \
  --output "$scratch_directory/MoltenVK-macos.tar" \
  "https://github.com/KhronosGroup/MoltenVK/releases/download/${MOLTENVK_RELEASE_TAG}/MoltenVK-macos.tar"
echo "${MOLTENVK_MACOS_TARBALL_SHA256}  $scratch_directory/MoltenVK-macos.tar" | shasum -a 256 --check
moltenvk_dylib_directory_in_tarball="MoltenVK/MoltenVK/dynamic/dylib/macOS"
tar -xf "$scratch_directory/MoltenVK-macos.tar" -C "$scratch_directory" "$moltenvk_dylib_directory_in_tarball"
lipo "$scratch_directory/$moltenvk_dylib_directory_in_tarball/libMoltenVK.dylib" \
  -thin arm64 -output "$bundled_vulkan_driver_directory/libMoltenVK.dylib"
codesign --force --sign - "$bundled_vulkan_driver_directory/libMoltenVK.dylib"
# `library_path` is `./libMoltenVK.dylib`, which the loader resolves against the
# manifest's own directory — so the manifest travels unedited.
cp "$scratch_directory/$moltenvk_dylib_directory_in_tarball/MoltenVK_icd.json" \
  "$bundled_vulkan_driver_directory/MoltenVK_icd.json"

echo "Vulkan-Loader ${VULKAN_LOADER_TAG}: built for arm64 at macOS ${macos_deployment_target}"
git clone --quiet --depth 1 --branch "$VULKAN_LOADER_TAG" \
  https://github.com/KhronosGroup/Vulkan-Loader "$scratch_directory/Vulkan-Loader"
cmake -S "$scratch_directory/Vulkan-Loader" -B "$scratch_directory/Vulkan-Loader/build" \
  -G Ninja \
  -D CMAKE_BUILD_TYPE=Release \
  -D CMAKE_OSX_ARCHITECTURES=arm64 \
  -D CMAKE_OSX_DEPLOYMENT_TARGET="$macos_deployment_target" \
  -D UPDATE_DEPS=ON \
  -D BUILD_TESTS=OFF
cmake --build "$scratch_directory/Vulkan-Loader/build" --target vulkan
# The build's `libvulkan.1.dylib` is a symlink to the versioned file; the wheel
# carries one real file under the name the engine opens.
cp -L "$scratch_directory/Vulkan-Loader/build/loader/libvulkan.1.dylib" \
  "$bundled_vulkan_driver_directory/libvulkan.1.dylib"
codesign --force --sign - "$bundled_vulkan_driver_directory/libvulkan.1.dylib"

for bundled_binary in "$bundled_vulkan_driver_directory"/*.dylib; do
  codesign --verify --strict "$bundled_binary"
done
ls -l "$bundled_vulkan_driver_directory"
