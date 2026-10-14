#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Stages the Vulkan loader and Mesa's lavapipe into the Linux runtime unit's
# package tree, so a machine with no Vulkan driver at all still has a device.
#
# Writes `libvulkan.so.1`, `libvulkan_lvp.so` and `lvp_icd.x86_64.json` into
# `sdk/streamlib-python-wheel/python/tatolab/runtime/_vulkan_driver/` (or the
# directory given as $1), replacing whatever was there.
#
# The driver is always built inside the pinned manylinux_2_28 image, so a
# developer's unit, CI's and the release carry the same build at the glibc 2.28
# floor. Outside that image the script re-runs itself in it through docker. The
# build is cached under target/, keyed by this script, the image pin and the
# Vulkan SDK tag.

set -euo pipefail

MESA_VERSION="26.2.4"
MESA_TARBALL_SHA256="bce5f7fbebb934373b86c999a064d52fb5065878dc57f287f95346648ec832e9"
ZSTD_VERSION="1.5.7"
ZSTD_TARBALL_SHA256="eb33e51f49a15e023950cd7825ca74a4a2b43db8354825ac24fc1b7ee09e6fa3"
LLVM_PACKAGE_VERSION="21.1.8"
MESA_BUILD_PYTHON_REQUIREMENTS=(
  "meson==1.12.1" "ninja==1.13.2" "mako==1.4.3" "markupsafe==3.0.4" "pyyaml==6.0.3" "packaging==26.3"
)

# What any glibc 2.28 Linux host supplies; a NEEDED outside this list is a
# library the bundled driver would demand of a machine that may not have it.
HOST_SUPPLIED_SHARED_LIBRARIES=(
  libc.so.6 libdl.so.2 libgcc_s.so.1 libm.so.6 libpthread.so.0 librt.so.1
  libstdc++.so.6 ld-linux-x86-64.so.2
)
GLIBC_FLOOR="2.28"

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source-path=SCRIPTDIR source=manylinux_2_28_image.sh
. "$workspace_root/scripts/manylinux_2_28_image.sh"

# The loader, and glslang beside it, come from the one Vulkan SDK release the
# macOS unit pins.
vulkan_sdk_tag="$(
  sed -n 's/^VULKAN_LOADER_TAG="\(.*\)"$/\1/p' "$workspace_root/scripts/stage_macos_bundled_vulkan_driver.sh"
)"
if [ -z "$vulkan_sdk_tag" ]; then
  echo "error: no VULKAN_LOADER_TAG in $workspace_root/scripts/stage_macos_bundled_vulkan_driver.sh" >&2
  exit 1
fi

fill_the_cache_only=false
if [ "${1:-}" = "--fill-the-cache-only" ]; then
  fill_the_cache_only=true
else
  bundled_vulkan_driver_directory="${1:-$workspace_root/sdk/streamlib-python-wheel/python/tatolab/runtime/_vulkan_driver}"
fi

cache_key="$(
  {
    cat "${BASH_SOURCE[0]}"
    echo "$MANYLINUX_2_28_IMAGE"
    echo "$vulkan_sdk_tag"
  } | sha256sum | cut -c1-16
)"
cache_root="$workspace_root/target/linux_bundled_vulkan_driver_cache"
cache_entry="$cache_root/$cache_key"

running_inside_the_pinned_image() {
  [ "${STREAMLIB_RUNNING_IN_MANYLINUX_2_28_IMAGE:-}" = "$MANYLINUX_2_28_IMAGE" ]
}

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
  echo "error: dnf install did not complete in three bounded attempts" >&2
  return 1
}

needed_shared_libraries_of() {
  readelf -d "$1" | sed -n 's/.*(NEEDED).*\[\(.*\)\]$/\1/p'
}

newest_glibc_symbol_version_of() {
  objdump -T "$1" | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/^GLIBC_//' | sort -uV | tail -n 1
}

refuse_unless_portable_to_every_glibc_floor_host() {
  local staged_directory="$1"
  local staged_file_names
  staged_file_names="$(find "$staged_directory" -mindepth 1 -maxdepth 1 -printf '%f\n' | LC_ALL=C sort | tr '\n' ' ')"
  if [ "$staged_file_names" != "libvulkan.so.1 libvulkan_lvp.so lvp_icd.x86_64.json " ]; then
    echo "error: $staged_directory holds '$staged_file_names', not exactly the three bundled files" >&2
    return 1
  fi
  if [ -L "$staged_directory/libvulkan.so.1" ]; then
    echo "error: $staged_directory/libvulkan.so.1 is a symlink, not one real file" >&2
    return 1
  fi
  if ! grep -q '"library_path": "./libvulkan_lvp.so"' "$staged_directory/lvp_icd.x86_64.json"; then
    echo "error: $staged_directory/lvp_icd.x86_64.json does not name ./libvulkan_lvp.so" >&2
    return 1
  fi
  local shared_library needed newest_glibc portable=true
  for shared_library in libvulkan.so.1 libvulkan_lvp.so; do
    echo "NEEDED by $shared_library:"
    while read -r needed; do
      echo "  $needed"
      if ! printf '%s\n' "${HOST_SUPPLIED_SHARED_LIBRARIES[@]}" | grep -qxF "$needed"; then
        echo "error: $shared_library needs $needed, which a host is not promised to supply" >&2
        portable=false
      fi
    done < <(needed_shared_libraries_of "$staged_directory/$shared_library")
    newest_glibc="$(newest_glibc_symbol_version_of "$staged_directory/$shared_library")"
    echo "  newest GLIBC_ symbol version: $newest_glibc"
    if [ "$(printf '%s\n%s\n' "$newest_glibc" "$GLIBC_FLOOR" | sort -V | tail -n 1)" != "$GLIBC_FLOOR" ]; then
      echo "error: $shared_library needs GLIBC_$newest_glibc, above the $GLIBC_FLOOR floor" >&2
      portable=false
    fi
  done
  "$portable"
}

replace_exactly_one_line() {
  python3 -I -c '
import sys
path, line_before, replacement = sys.argv[1:]
lines = open(path).read().split("\n")
if lines.count(line_before) != 1:
    sys.exit(f"error: {path} does not hold exactly one line {line_before!r} to patch")
lines[lines.index(line_before)] = replacement
open(path, "w").write("\n".join(lines))
' "$@"
}

build_the_bundled_vulkan_driver_into() {
  local staged_directory="$1"
  local scratch_directory
  scratch_directory="$(mktemp -d)"
  # shellcheck disable=SC2064
  trap "rm -rf '$scratch_directory'" EXIT

  install_build_dependencies_with_bounded_retry \
    "llvm-devel-$LLVM_PACKAGE_VERSION" "llvm-static-$LLVM_PACKAGE_VERSION" \
    bison flex zlib-static libxml2-devel libXrandr-devel

  uv venv --quiet --python /usr/local/bin/python3.12 "$scratch_directory/build-venv"
  VIRTUAL_ENV="$scratch_directory/build-venv" uv pip install --quiet "${MESA_BUILD_PYTHON_REQUIREMENTS[@]}"
  export PATH="$scratch_directory/build-venv/bin:$PATH"

  # `llvm-config --link-static --system-libs` asks for -lz, -lzstd and -lxml2,
  # and the image carries only their shared libraries. A directory of static
  # archives, searched before the system's, links zlib and zstd into the driver
  # rather than leaving a NEEDED a host may not satisfy; no libzstd-static
  # package exists. Nothing lavapipe links references libxml2, so --as-needed
  # drops it, and libxml2-devel is there only so -lxml2 resolves. The NEEDED
  # check below proves both.
  local static_archive_directory="$scratch_directory/static-archives-before-the-system-libraries"
  mkdir -p "$static_archive_directory"
  cp /usr/lib64/libz.a "$static_archive_directory/"
  echo "zstd ${ZSTD_VERSION}: a static PIC archive for LLVM's compression support"
  curl --fail --silent --show-error --location --retry 4 \
    --output "$scratch_directory/zstd.tar.gz" \
    "https://github.com/facebook/zstd/releases/download/v${ZSTD_VERSION}/zstd-${ZSTD_VERSION}.tar.gz"
  echo "${ZSTD_TARBALL_SHA256}  $scratch_directory/zstd.tar.gz" | sha256sum --check --quiet
  tar -xzf "$scratch_directory/zstd.tar.gz" -C "$scratch_directory"
  make -C "$scratch_directory/zstd-${ZSTD_VERSION}/lib" -j "$(nproc)" libzstd.a \
    CFLAGS="-O2 -fPIC -DZSTD_TRACE=0" >/dev/null
  cp "$scratch_directory/zstd-${ZSTD_VERSION}/lib/libzstd.a" "$static_archive_directory/"

  # Lavapipe's acceleration-structure shaders compile with glslangValidator,
  # which AlmaLinux 8 does not package.
  echo "glslang ${vulkan_sdk_tag}: glslangValidator for lavapipe's BVH shaders"
  git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$vulkan_sdk_tag" \
    https://github.com/KhronosGroup/glslang "$scratch_directory/glslang"
  cmake -S "$scratch_directory/glslang" -B "$scratch_directory/glslang/build" -G Ninja \
    -D CMAKE_BUILD_TYPE=Release \
    -D CMAKE_INSTALL_PREFIX="$scratch_directory/glslang-install" \
    -D ENABLE_OPT=OFF \
    -D ENABLE_HLSL=OFF \
    -D GLSLANG_TESTS=OFF >/dev/null
  ninja -C "$scratch_directory/glslang/build" install >/dev/null
  export PATH="$scratch_directory/glslang-install/bin:$PATH"

  echo "Mesa ${MESA_VERSION}: lavapipe, headless, LLVM ${LLVM_PACKAGE_VERSION} linked statically"
  curl --fail --silent --show-error --location --retry 4 \
    --output "$scratch_directory/mesa.tar.xz" \
    "https://archive.mesa3d.org/mesa-${MESA_VERSION}.tar.xz"
  echo "${MESA_TARBALL_SHA256}  $scratch_directory/mesa.tar.xz" | sha256sum --check --quiet
  tar -xJf "$scratch_directory/mesa.tar.xz" -C "$scratch_directory"
  local mesa_source_directory="$scratch_directory/mesa-${MESA_VERSION}"
  # Mesa ties libdrm to every Vulkan driver on Linux: its configure requires
  # libdrm, and builds the DRM display WSI, on any KMS/DRM host. Lavapipe's code
  # builds without it (HAVE_LIBDRM gates only its sync-file, DMA-BUF and
  # modifier support) once it takes DRM_FORMAT_MOD_LINEAR from Mesa's in-tree
  # copy of the kernel header rather than through libdrm. Each patch must apply
  # exactly, so a Mesa bump re-examines it rather than skipping it.
  replace_exactly_one_line "$mesa_source_directory/meson.build" \
    "system_has_kms_drm = ['openbsd', 'netbsd', 'freebsd', 'gnu/kfreebsd', 'dragonfly', 'linux', 'sunos', 'android', 'managarm'].contains(host_machine.system())" \
    "system_has_kms_drm = false"
  replace_exactly_one_line "$mesa_source_directory/src/gallium/frontends/lavapipe/lvp_formats.c" \
    '#include "lvp_private.h"' \
    $'#include "lvp_private.h"\n#include "drm-uapi/drm_fourcc.h"'

  # Headless by the owner's ruling: no window system, no DMA-BUF, no sync file.
  # The image already carries libdrm-devel, libudev, valgrind and the X11
  # stack's .pc files, and Mesa links whatever optional dependency its
  # configure finds, so pkg-config sees zlib alone.
  local pkg_config_directory_visible_to_mesa="$scratch_directory/pkg-config-visible-to-mesa"
  mkdir -p "$pkg_config_directory_visible_to_mesa"
  cp /usr/lib64/pkgconfig/zlib.pc "$pkg_config_directory_visible_to_mesa/"
  PKG_CONFIG_PATH="" PKG_CONFIG_LIBDIR="$pkg_config_directory_visible_to_mesa" \
    LDFLAGS="-L$static_archive_directory" \
    meson setup "$mesa_source_directory/build" "$mesa_source_directory" \
    --wrap-mode=nofallback \
    -D buildtype=release \
    -D b_ndebug=true \
    -D platforms= \
    -D vulkan-drivers=swrast \
    -D gallium-drivers=llvmpipe \
    -D llvm=enabled \
    -D shared-llvm=disabled \
    -D zstd=disabled \
    -D xmlconfig=disabled \
    -D expat=disabled \
    -D opengl=false \
    -D glx=disabled \
    -D egl=disabled \
    -D gbm=disabled \
    -D gles1=disabled \
    -D gles2=disabled \
    -D glvnd=disabled \
    -D libunwind=disabled \
    -D valgrind=disabled \
    -D lmsensors=disabled \
    -D display-info=disabled \
    -D spirv-tools=disabled \
    -D video-codecs= \
    -D vulkan-layers= \
    -D tools= \
    -D build-tests=false
  if grep -q -- '-DHAVE_LIBDRM' "$mesa_source_directory/build/build.ninja"; then
    echo "error: Mesa's configure found libdrm; the headless build must not see it" >&2
    return 1
  fi
  ninja -C "$mesa_source_directory/build" src/gallium/targets/lavapipe/libvulkan_lvp.so \
    src/gallium/targets/lavapipe/lvp_icd.x86_64.json

  echo "Vulkan-Loader ${vulkan_sdk_tag}"
  git -c advice.detachedHead=false clone --quiet --depth 1 --branch "$vulkan_sdk_tag" \
    https://github.com/KhronosGroup/Vulkan-Loader "$scratch_directory/Vulkan-Loader"
  cmake -S "$scratch_directory/Vulkan-Loader" -B "$scratch_directory/Vulkan-Loader/build" -G Ninja \
    -D CMAKE_BUILD_TYPE=Release \
    -D UPDATE_DEPS=ON \
    -D BUILD_TESTS=OFF >/dev/null
  cmake --build "$scratch_directory/Vulkan-Loader/build" --target vulkan

  mkdir -p "$staged_directory"
  # The build's `libvulkan.so.1` is a symlink to the versioned file; the unit
  # carries one real file under the name the engine opens.
  cp -L "$scratch_directory/Vulkan-Loader/build/loader/libvulkan.so.1" "$staged_directory/libvulkan.so.1"
  cp "$mesa_source_directory/build/src/gallium/targets/lavapipe/libvulkan_lvp.so" "$staged_directory/"
  strip --strip-unneeded "$staged_directory/libvulkan.so.1" "$staged_directory/libvulkan_lvp.so"
  # Mesa writes `library_path` as the absolute <prefix>/<libdir> it would
  # install to. `./libvulkan_lvp.so` is what MoltenVK's manifest does: the
  # loader resolves it against the manifest's own directory, wherever the unit
  # is installed.
  python3 -I -c '
import json, sys
manifest = json.load(open(sys.argv[1]))
manifest["ICD"]["library_path"] = "./libvulkan_lvp.so"
with open(sys.argv[2], "w") as staged_manifest:
    json.dump(manifest, staged_manifest, indent=4, sort_keys=True, separators=(",", ": "))
' "$mesa_source_directory/build/src/gallium/targets/lavapipe/lvp_icd.x86_64.json" \
    "$staged_directory/lvp_icd.x86_64.json"

  refuse_unless_portable_to_every_glibc_floor_host "$staged_directory"
}

if [ -d "$cache_entry" ]; then
  echo "bundled Vulkan driver: cache hit at $cache_entry"
else
  echo "bundled Vulkan driver: cache miss, building into $cache_entry"
  mkdir -p "$cache_root"
  if running_inside_the_pinned_image; then
    staged_outside_the_workspace="$(mktemp -d)/staged"
    build_the_bundled_vulkan_driver_into "$staged_outside_the_workspace"
    partial_cache_entry="$cache_entry.partial.$$"
    cp -R "$staged_outside_the_workspace" "$partial_cache_entry"
    mv "$partial_cache_entry" "$cache_entry"
    if [ -n "${BUNDLED_VULKAN_DRIVER_CACHE_OWNER:-}" ]; then
      chown "$BUNDLED_VULKAN_DRIVER_CACHE_OWNER" "$cache_root"
      chown -R "$BUNDLED_VULKAN_DRIVER_CACHE_OWNER" "$cache_entry"
    fi
  else
    if ! command -v docker >/dev/null 2>&1; then
      echo "error: the bundled Vulkan driver builds inside $MANYLINUX_2_28_IMAGE, and docker is not on PATH;" \
        "a container already on that image says so with STREAMLIB_RUNNING_IN_MANYLINUX_2_28_IMAGE" >&2
      exit 1
    fi
    docker run --rm \
      --volume "$workspace_root:/workspace" \
      --workdir /workspace \
      --env STREAMLIB_RUNNING_IN_MANYLINUX_2_28_IMAGE="$MANYLINUX_2_28_IMAGE" \
      --env BUNDLED_VULKAN_DRIVER_CACHE_OWNER="$(id -u):$(id -g)" \
      "$MANYLINUX_2_28_IMAGE" \
      bash "/workspace/scripts/$(basename "${BASH_SOURCE[0]}")" --fill-the-cache-only
    if [ ! -d "$cache_entry" ]; then
      echo "error: the build inside $MANYLINUX_2_28_IMAGE left no $cache_entry" >&2
      exit 1
    fi
  fi
fi

if "$fill_the_cache_only"; then
  exit 0
fi

rm -rf "$bundled_vulkan_driver_directory"
mkdir -p "$bundled_vulkan_driver_directory"
cp "$cache_entry/libvulkan.so.1" "$cache_entry/libvulkan_lvp.so" "$cache_entry/lvp_icd.x86_64.json" \
  "$bundled_vulkan_driver_directory/"
ls -l "$bundled_vulkan_driver_directory"
