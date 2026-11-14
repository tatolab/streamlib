#!/usr/bin/env bash
# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
#
# Stages the Slang shader compiler the engine opens at run time:
#
#   scripts/stage_slang_compiler_libraries.sh [<directory>...]
#   scripts/stage_slang_compiler_libraries.sh --fill-the-cache-only
#
# writes `libslang-compiler` (under its unversioned name) and the optimiser
# `libslang-glslang` into
# `sdk/streamlib-python-wheel/python/tatolab/runtime/_slang_compiler/`, where
# the wheel carries them beside `_engine`, and into `target/slang_compiler/`,
# where a debug build's `cargo test` finds them; or into each directory given,
# replacing what was there.
#
# On Linux both are built from Slang's sources at the pinned tag inside the
# pinned manylinux_2_28 image, so a developer's unit, CI's and the release carry
# one build at the glibc 2.28 and libstdc++ 3.4.25 floor; Slang's own Linux
# release needs a newer libstdc++. Outside that image the script re-runs itself
# in it through docker. On macOS both come from Slang's release archive, pinned
# by checksum. Either is cached under target/slang_compiler_cache/, and a
# library above the floor is refused.

set -euo pipefail

SLANG_VERSION="2026.12"
SLANG_SOURCE_REPOSITORY="https://github.com/shader-slang/slang"
SLANG_SOURCE_COMMIT="a7fbf1ab0e9ddd18b0a9eaa675ddc95f63532fee"
# What the two libraries compile from; the rest of Slang's submodules serve
# targets this build turns off.
SLANG_SOURCE_SUBMODULES_THE_LIBRARIES_COMPILE=(
  external/glslang external/spirv-tools external/spirv-headers external/miniz external/lz4
  external/unordered_dense external/vulkan external/cmark external/fast_float external/lua
)
SLANG_BUILD_PYTHON_REQUIREMENTS=("cmake==3.31.6" "ninja==1.13.2")
SLANG_RELEASE_DOWNLOAD_URL="https://github.com/shader-slang/slang/releases/download/v$SLANG_VERSION"
SLANG_MACOS_RELEASE_ARCHIVE_NAME="slang-$SLANG_VERSION-macos-aarch64.tar.gz"
SLANG_MACOS_RELEASE_ARCHIVE_SHA256="018a425a8d6a34b172324db2cf5097e3789d232136f1e6e01976eb4196045cb2"

GLIBC_FLOOR="2.28"
GLIBCXX_FLOOR="3.4.25"
MACOS_MINIMUM_FLOOR="26.0"

# What any manylinux_2_28 host supplies; a NEEDED outside this list is a
# library the compiler would demand of a machine that may not have it.
HOST_SUPPLIED_SHARED_LIBRARIES=(
  libc.so.6 libdl.so.2 libgcc_s.so.1 libm.so.6 libpthread.so.0 librt.so.1
  libstdc++.so.6 ld-linux-x86-64.so.2
)

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source-path=SCRIPTDIR source=manylinux_2_28_image.sh
. "$workspace_root/scripts/manylinux_2_28_image.sh"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) shared_library_suffix="so" ;;
  Darwin-arm64) shared_library_suffix="dylib" ;;
  *)
    echo "error: no pinned Slang build for $(uname -s) $(uname -m); the runtime unit is built on Linux x86_64 and Apple Silicon macOS" >&2
    exit 1
    ;;
esac
slang_compiler_library_file_name="libslang-compiler.$shared_library_suffix"
slang_spirv_optimiser_library_file_name="libslang-glslang-$SLANG_VERSION.$shared_library_suffix"

fill_the_cache_only=false
if [ "${1:-}" = "--fill-the-cache-only" ]; then
  fill_the_cache_only=true
  staging_directories=()
elif [ "$#" -gt 0 ]; then
  staging_directories=("$@")
else
  staging_directories=(
    "$workspace_root/sdk/streamlib-python-wheel/python/tatolab/runtime/_slang_compiler"
    "$workspace_root/target/slang_compiler"
  )
fi

sha256_of_standard_input() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum
  else
    shasum -a 256
  fi
}

cache_key="$(
  {
    cat "${BASH_SOURCE[0]}"
    echo "$MANYLINUX_2_28_IMAGE"
    uname -s
  } | sha256_of_standard_input | cut -c1-16
)"
cache_root="$workspace_root/target/slang_compiler_cache"
cache_entry="$cache_root/$cache_key"

running_inside_the_pinned_image() {
  [ "${STREAMLIB_RUNNING_IN_MANYLINUX_2_28_IMAGE:-}" = "$MANYLINUX_2_28_IMAGE" ]
}

# An entry counts only with both libraries in it: a restored `target/` can
# carry the entry's directory with nothing in it.
cache_entry_holds_both_libraries() {
  [ -f "$cache_entry/$slang_compiler_library_file_name" ] &&
    [ -f "$cache_entry/$slang_spirv_optimiser_library_file_name" ]
}

newest_symbol_version_of() {
  local symbol_prefix="$1" shared_library="$2"
  objdump -T "$shared_library" | grep -o "${symbol_prefix}_[0-9][0-9.]*" | sed "s/^${symbol_prefix}_//" | sort -uV | tail -n 1
}

at_or_below() {
  [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n 1)" = "$2" ]
}

refuse_unless_portable_to_every_linux_floor_host() {
  local shared_library="$1" needed newest_glibc newest_glibcxx portable=true
  echo "NEEDED by $(basename "$shared_library"):"
  while read -r needed; do
    echo "  $needed"
    if ! printf '%s\n' "${HOST_SUPPLIED_SHARED_LIBRARIES[@]}" | grep -qxF "$needed"; then
      echo "error: $(basename "$shared_library") needs $needed, which a host is not promised to supply" >&2
      portable=false
    fi
  done < <(readelf -d "$shared_library" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p')
  newest_glibc="$(newest_symbol_version_of GLIBC "$shared_library")"
  echo "  newest GLIBC_ symbol version: $newest_glibc"
  if ! at_or_below "$newest_glibc" "$GLIBC_FLOOR"; then
    echo "error: $(basename "$shared_library") needs GLIBC_$newest_glibc, above the $GLIBC_FLOOR floor" >&2
    portable=false
  fi
  newest_glibcxx="$(newest_symbol_version_of GLIBCXX "$shared_library")"
  echo "  newest GLIBCXX_ symbol version: ${newest_glibcxx:-none}"
  if [ -n "$newest_glibcxx" ] && ! at_or_below "$newest_glibcxx" "$GLIBCXX_FLOOR"; then
    echo "error: $(basename "$shared_library") needs GLIBCXX_$newest_glibcxx, above the $GLIBCXX_FLOOR floor" >&2
    portable=false
  fi
  "$portable"
}

refuse_unless_portable_to_every_macos_floor_host() {
  local shared_library="$1" macos_minimum
  macos_minimum="$(otool -l "$shared_library" | awk '/LC_BUILD_VERSION/ { in_build_version = 1 } in_build_version && $1 == "minos" { print $2; exit }')"
  echo "$(basename "$shared_library") macOS minimum: ${macos_minimum:-unknown}"
  if [ -z "$macos_minimum" ] || ! at_or_below "$macos_minimum" "$MACOS_MINIMUM_FLOOR"; then
    echo "error: $(basename "$shared_library") needs macOS ${macos_minimum:-of an unknown version}, above the $MACOS_MINIMUM_FLOOR floor" >&2
    return 1
  fi
  # An arm64 dylib whose signature does not verify loads on no Mac but the one
  # that built it.
  codesign --verify --strict "$shared_library"
}

refuse_unless_both_libraries_are_portable_in() {
  local directory="$1" library portable=true
  for library in "$slang_compiler_library_file_name" "$slang_spirv_optimiser_library_file_name"; do
    if [ "$shared_library_suffix" = "so" ]; then
      refuse_unless_portable_to_every_linux_floor_host "$directory/$library" || portable=false
    else
      refuse_unless_portable_to_every_macos_floor_host "$directory/$library" || portable=false
    fi
  done
  "$portable"
}

build_both_libraries_from_source_into() {
  local built_directory="$1"
  local scratch_directory
  scratch_directory="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$scratch_directory'" EXIT

  echo "Slang $SLANG_VERSION: the compiler and its optimiser, from source at $SLANG_SOURCE_COMMIT"
  git -c advice.detachedHead=false clone --quiet --depth 1 --branch "v$SLANG_VERSION" \
    "$SLANG_SOURCE_REPOSITORY" "$scratch_directory/slang"
  if [ "$(git -C "$scratch_directory/slang" rev-parse HEAD)" != "$SLANG_SOURCE_COMMIT" ]; then
    echo "error: Slang's v$SLANG_VERSION tag no longer names $SLANG_SOURCE_COMMIT" >&2
    return 1
  fi
  git -C "$scratch_directory/slang" submodule update --quiet --init --depth 1 \
    "${SLANG_SOURCE_SUBMODULES_THE_LIBRARIES_COMPILE[@]}"

  uv venv --quiet --python /usr/local/bin/python3.12 "$scratch_directory/build-venv"
  VIRTUAL_ENV="$scratch_directory/build-venv" uv pip install --quiet "${SLANG_BUILD_PYTHON_REQUIREMENTS[@]}"
  export PATH="$scratch_directory/build-venv/bin:$PATH"

  # Every option that would fetch a binary or build a target the runtime does
  # not open is off, so the build reaches nothing beyond the pinned sources.
  cmake -S "$scratch_directory/slang" -B "$scratch_directory/build" -G Ninja \
    -D CMAKE_BUILD_TYPE=Release \
    -D SLANG_SLANG_LLVM_FLAVOR=DISABLE \
    -D SLANG_ENABLE_DXIL=OFF \
    -D SLANG_ENABLE_SLANG_RHI=OFF \
    -D SLANG_ENABLE_GFX=OFF \
    -D SLANG_ENABLE_TESTS=OFF \
    -D SLANG_ENABLE_EXAMPLES=OFF \
    -D SLANG_ENABLE_SLANGD=OFF \
    -D SLANG_ENABLE_SLANGI=OFF \
    -D SLANG_ENABLE_SLANGRT=OFF \
    -D SLANG_ENABLE_REPLAYER=OFF \
    -D SLANG_ENABLE_SLANG_PROXY=OFF \
    -D SLANG_ENABLE_CUDA=OFF \
    -D SLANG_ENABLE_OPTIX=OFF \
    -D SLANG_ENABLE_NVAPI=OFF \
    -D SLANG_ENABLE_AFTERMATH=OFF \
    -D SLANG_ENABLE_XLIB=OFF \
    -D SLANG_ENABLE_SPIRV_TOOLS_MIMALLOC=OFF \
    -D SLANG_ENABLE_RELEASE_DEBUG_INFO=OFF \
    -D SLANG_EXCLUDE_TINT=ON >/dev/null
  ninja -C "$scratch_directory/build" \
    "Release/lib/libslang-compiler.so.0.$SLANG_VERSION" \
    "Release/lib/$slang_spirv_optimiser_library_file_name"

  mkdir -p "$built_directory"
  cp "$scratch_directory/build/Release/lib/libslang-compiler.so.0.$SLANG_VERSION" \
    "$built_directory/$slang_compiler_library_file_name"
  cp "$scratch_directory/build/Release/lib/$slang_spirv_optimiser_library_file_name" "$built_directory/"
  strip --strip-unneeded "$built_directory/$slang_compiler_library_file_name" \
    "$built_directory/$slang_spirv_optimiser_library_file_name"
  refuse_unless_both_libraries_are_portable_in "$built_directory"
}

fetch_both_libraries_from_the_macos_release_into() {
  local released_directory="$1"
  local scratch_directory
  scratch_directory="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$scratch_directory'" EXIT

  curl --proto '=https' --tlsv1.2 -sSfL --retry 5 \
    "$SLANG_RELEASE_DOWNLOAD_URL/$SLANG_MACOS_RELEASE_ARCHIVE_NAME" \
    -o "$scratch_directory/$SLANG_MACOS_RELEASE_ARCHIVE_NAME"
  if [ "$(sha256_of_standard_input <"$scratch_directory/$SLANG_MACOS_RELEASE_ARCHIVE_NAME" | cut -d' ' -f1)" != \
    "$SLANG_MACOS_RELEASE_ARCHIVE_SHA256" ]; then
    echo "error: $SLANG_MACOS_RELEASE_ARCHIVE_NAME is not the pinned Slang $SLANG_VERSION release archive" >&2
    return 1
  fi
  mkdir -p "$scratch_directory/extracted"
  tar -xzf "$scratch_directory/$SLANG_MACOS_RELEASE_ARCHIVE_NAME" -C "$scratch_directory/extracted"

  # The release names its compiler with its version and links the unversioned
  # names to it; the runtime opens it by the unversioned name.
  local pattern matches
  mkdir -p "$released_directory"
  for pattern in "libslang-compiler*.dylib" "$slang_spirv_optimiser_library_file_name"; do
    matches="$(find "$scratch_directory/extracted" -type f -name "$pattern")"
    if [ "$(printf '%s\n' "$matches" | grep -c .)" != "1" ]; then
      echo "error: the Slang $SLANG_VERSION archive holds no single regular file named $pattern:" >&2
      printf '%s\n' "$matches" >&2
      return 1
    fi
    if [ "$pattern" = "$slang_spirv_optimiser_library_file_name" ]; then
      cp "$matches" "$released_directory/$slang_spirv_optimiser_library_file_name"
    else
      cp "$matches" "$released_directory/$slang_compiler_library_file_name"
    fi
  done
  refuse_unless_both_libraries_are_portable_in "$released_directory"
}

if cache_entry_holds_both_libraries; then
  echo "Slang compiler: cache hit at $cache_entry"
else
  if [ -e "$cache_entry" ]; then
    echo "Slang compiler: $cache_entry is incomplete; removing it to fill it again"
    rm -rf "$cache_entry"
  fi
  echo "Slang compiler: cache miss, filling $cache_entry"
  mkdir -p "$cache_root"
  if [ "$shared_library_suffix" = "dylib" ] || running_inside_the_pinned_image; then
    filled_outside_the_workspace="$(mktemp -d)/filled"
    if [ "$shared_library_suffix" = "dylib" ]; then
      fetch_both_libraries_from_the_macos_release_into "$filled_outside_the_workspace"
    else
      build_both_libraries_from_source_into "$filled_outside_the_workspace"
    fi
    partial_cache_entry="$cache_entry.partial.$$"
    cp -R "$filled_outside_the_workspace" "$partial_cache_entry"
    mv "$partial_cache_entry" "$cache_entry"
    if [ -n "${SLANG_COMPILER_CACHE_OWNER:-}" ]; then
      chown "$SLANG_COMPILER_CACHE_OWNER" "$cache_root"
      chown -R "$SLANG_COMPILER_CACHE_OWNER" "$cache_entry"
    fi
  else
    if ! command -v docker >/dev/null 2>&1; then
      echo "error: the Slang compiler builds inside $MANYLINUX_2_28_IMAGE, and docker is not on PATH;" \
        "a container already on that image says so with STREAMLIB_RUNNING_IN_MANYLINUX_2_28_IMAGE" >&2
      exit 1
    fi
    docker run --rm \
      --volume "$workspace_root:/workspace" \
      --workdir /workspace \
      --env STREAMLIB_RUNNING_IN_MANYLINUX_2_28_IMAGE="$MANYLINUX_2_28_IMAGE" \
      --env SLANG_COMPILER_CACHE_OWNER="$(id -u):$(id -g)" \
      "$MANYLINUX_2_28_IMAGE" \
      bash "/workspace/scripts/$(basename "${BASH_SOURCE[0]}")" --fill-the-cache-only
    if ! cache_entry_holds_both_libraries; then
      echo "error: the build inside $MANYLINUX_2_28_IMAGE left no complete $cache_entry" >&2
      exit 1
    fi
  fi
fi

if "$fill_the_cache_only"; then
  exit 0
fi

for staging_directory in "${staging_directories[@]}"; do
  rm -rf "$staging_directory"
  mkdir -p "$staging_directory"
  cp "$cache_entry/$slang_compiler_library_file_name" "$cache_entry/$slang_spirv_optimiser_library_file_name" \
    "$staging_directory/"
  echo "staged Slang $SLANG_VERSION into $staging_directory"
done
