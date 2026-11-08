#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Stages the Slang shader compiler the engine opens at run time:
#
#   scripts/stage_slang_compiler_libraries.sh [<directory>...]
#
# writes `libslang-compiler` (under its unversioned name) and the optimiser
# `libslang-glslang` into
# `sdk/streamlib-python-wheel/python/tatolab/runtime/_slang_compiler/`, where
# the wheel carries them beside `_engine`, and into `target/slang_compiler/`,
# where a debug build's `cargo test` finds them; or into each directory given,
# replacing what was there.
#
# Both come from Slang's own release archive, pinned here by version and
# checksum and cached under target/. A library whose glibc or libstdc++ floor
# is above manylinux_2_28's, or whose macOS minimum is above 26.0, is refused:
# that floor has to build Slang from source instead.

set -euo pipefail

SLANG_VERSION="2026.12"
SLANG_RELEASE_DOWNLOAD_URL="https://github.com/shader-slang/slang/releases/download/v$SLANG_VERSION"

GLIBC_FLOOR="2.28"
GLIBCXX_FLOOR="3.4.25"
MACOS_MINIMUM_FLOOR="26.0"

# What any manylinux_2_28 host supplies; a NEEDED outside this list is a
# library the compiler would demand of a machine that may not have it.
HOST_SUPPLIED_SHARED_LIBRARIES=(
  libc.so.6 libdl.so.2 libgcc_s.so.1 libm.so.6 libpthread.so.0 librt.so.1
  libstdc++.so.6 ld-linux-x86-64.so.2 ld-linux-aarch64.so.1
)

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    slang_release_archive_name="slang-$SLANG_VERSION-linux-x86_64-glibc-2.27.tar.gz"
    slang_release_archive_sha256="e5d0340c82fc9f5cb2858d6a64c3ad857a18bc8664a0d59915face98c96a80c6"
    shared_library_suffix="so"
    ;;
  Linux-aarch64)
    slang_release_archive_name="slang-$SLANG_VERSION-linux-aarch64-glibc-2.28.tar.gz"
    slang_release_archive_sha256="f1aa942876582dbbb4a3ea57f6a490da82ed6f61ac99353a76323296071b2a50"
    shared_library_suffix="so"
    ;;
  Darwin-arm64)
    slang_release_archive_name="slang-$SLANG_VERSION-macos-aarch64.tar.gz"
    slang_release_archive_sha256="018a425a8d6a34b172324db2cf5097e3789d232136f1e6e01976eb4196045cb2"
    shared_library_suffix="dylib"
    ;;
  *)
    echo "error: no pinned Slang release for $(uname -s) $(uname -m); the runtime unit is built on Linux x86_64 or aarch64 and Apple Silicon macOS" >&2
    exit 1
    ;;
esac

slang_compiler_library_file_name="libslang-compiler.$shared_library_suffix"
slang_spirv_optimiser_library_file_name="libslang-glslang-$SLANG_VERSION.$shared_library_suffix"

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ "$#" -gt 0 ]; then
  staging_directories=("$@")
else
  staging_directories=(
    "$workspace_root/sdk/streamlib-python-wheel/python/tatolab/runtime/_slang_compiler"
    "$workspace_root/target/slang_compiler"
  )
fi

cache_entry="$workspace_root/target/slang_compiler_release_cache/$slang_release_archive_sha256"
cached_archive="$cache_entry/$slang_release_archive_name"
extracted_release="$cache_entry/extracted"

if [ ! -f "$cached_archive" ]; then
  mkdir -p "$cache_entry"
  curl --proto '=https' --tlsv1.2 -sSfL --retry 5 \
    "$SLANG_RELEASE_DOWNLOAD_URL/$slang_release_archive_name" -o "$cached_archive.partial"
  mv "$cached_archive.partial" "$cached_archive"
fi
if ! echo "$slang_release_archive_sha256  $cached_archive" | shasum -a 256 --check --quiet; then
  echo "error: $cached_archive is not the pinned Slang $SLANG_VERSION release archive" >&2
  rm -f "$cached_archive"
  exit 1
fi
rm -rf "$extracted_release"
mkdir -p "$extracted_release"
tar -xzf "$cached_archive" -C "$extracted_release"

# The release names its compiler with its version and links the unversioned
# names to it; the runtime opens it by the unversioned name.
the_one_regular_file_named() {
  local pattern="$1"
  local matches
  matches="$(find "$extracted_release" -type f -name "$pattern")"
  if [ "$(printf '%s\n' "$matches" | grep -c .)" != "1" ]; then
    echo "error: the Slang $SLANG_VERSION archive holds no single regular file named $pattern:" >&2
    printf '%s\n' "$matches" >&2
    return 1
  fi
  printf '%s\n' "$matches"
}
released_slang_compiler_library="$(the_one_regular_file_named "libslang-compiler*.$shared_library_suffix*")"
released_slang_spirv_optimiser_library="$(the_one_regular_file_named "$slang_spirv_optimiser_library_file_name")"

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
}

portable=true
for released_library in "$released_slang_compiler_library" "$released_slang_spirv_optimiser_library"; do
  if [ "$shared_library_suffix" = "so" ]; then
    refuse_unless_portable_to_every_linux_floor_host "$released_library" || portable=false
  else
    refuse_unless_portable_to_every_macos_floor_host "$released_library" || portable=false
  fi
done
if ! "$portable"; then
  echo "error: the Slang $SLANG_VERSION release does not run on every host of this floor; build Slang from source for it instead" >&2
  exit 1
fi

for staging_directory in "${staging_directories[@]}"; do
  rm -rf "$staging_directory"
  mkdir -p "$staging_directory"
  cp "$released_slang_compiler_library" "$staging_directory/$slang_compiler_library_file_name"
  cp "$released_slang_spirv_optimiser_library" "$staging_directory/$slang_spirv_optimiser_library_file_name"
  if [ "$shared_library_suffix" = "dylib" ]; then
    # An arm64 dylib whose signature does not verify loads on no Mac but the
    # one that built it.
    for staged_library in "$staging_directory"/*.dylib; do
      codesign --verify --strict "$staged_library"
    done
  fi
  echo "staged Slang $SLANG_VERSION into $staging_directory"
done
