#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Runs the Linux runtime tarball on two machines with no Vulkan driver and
# starts a stream on each:
#
#   scripts/check_linux_runtime_tarball_on_a_machine_with_no_vulkan_driver.sh <tarball>
#
# one with no Vulkan loader either, and an Ubuntu 22.04 whose system loader
# (1.3.204) predates the `VK_ADD_DRIVER_FILES` that names the bundled driver.
# Passes when on both `tatolabd` opened the Vulkan loader the lend carries and
# the first stream start built its device on the lavapipe beside it. What the
# stream does after the device is built is not checked here. Needs docker.

set -euo pipefail

# python:3.12-slim-bookworm as of 2026-10-13: a Debian with Python and nothing
# of Vulkan, pinned so the check runs against the image it last passed on.
MACHINE_WITH_NO_VULKAN_DRIVER_IMAGE="python@sha256:34386ef0cb081344d7ec1c103ba398e6e9f64e9ab3a1509accc92a4e24a07258"
# ubuntu:22.04 as of 2026-10-14, given its own libvulkan1 and no driver.
MACHINE_WITH_AN_OLD_SYSTEM_VULKAN_LOADER_IMAGE="ubuntu@sha256:5ec03bb3441e8b0bf3b4f9cd4629a1ae763010dc3035bb8da3ae6cf026486401"
OLD_SYSTEM_VULKAN_LOADER_PACKAGE_VERSION="1.3.204.1-2"

workspace_root="$(cd "$(dirname "$0")/.." && pwd)"
script_name="$(basename "$0")"

case "${1:-}" in
--inside-the-machine-with-no-vulkan-loader | --inside-the-machine-with-an-old-system-vulkan-loader)
	machine="$1"
	;;
*)
	runtime_tarball="${1:?usage: $script_name <linux-runtime-tarball>}"
	runtime_tarball="$(cd "$(dirname "$runtime_tarball")" && pwd)/$(basename "$runtime_tarball")"
	for machine_and_image in \
		"--inside-the-machine-with-no-vulkan-loader $MACHINE_WITH_NO_VULKAN_DRIVER_IMAGE" \
		"--inside-the-machine-with-an-old-system-vulkan-loader $MACHINE_WITH_AN_OLD_SYSTEM_VULKAN_LOADER_IMAGE"; do
		read -r machine image <<<"$machine_and_image"
		echo "=== $machine"
		docker run --rm \
			--volume "$runtime_tarball:/linux-runtime-tarball.tar.gz:ro" \
			--volume "$workspace_root:/workspace:ro" \
			"$image" \
			bash "/workspace/scripts/$script_name" "$machine"
	done
	exit 0
	;;
esac

if [ "$machine" = --inside-the-machine-with-an-old-system-vulkan-loader ]; then
	apt-get update -qq
	DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
		"libvulkan1=$OLD_SYSTEM_VULKAN_LOADER_PACKAGE_VERSION" python3 python3-venv python3-pip >/dev/null
	if [ ! -e /usr/lib/x86_64-linux-gnu/libvulkan.so.1 ]; then
		echo "error: the image carries no system Vulkan loader to be passed over" >&2
		exit 1
	fi
	# The package makes the driver directories; a driver is a manifest in one.
	vulkan_driver_presences=(/usr/share/vulkan/icd.d/*.json /etc/vulkan/icd.d/*.json)
else
	vulkan_driver_presences=(/usr/share/vulkan /etc/vulkan /usr/lib/x86_64-linux-gnu/libvulkan.so.1)
fi
for vulkan_presence in "${vulkan_driver_presences[@]}"; do
	if [ -e "$vulkan_presence" ]; then
		echo "error: the image carries $vulkan_presence, so it is not a machine with no Vulkan driver" >&2
		exit 1
	fi
done

runtime_unit_root=/opt/tatolab-runtime
mkdir -p "$runtime_unit_root"
tar -xzf /linux-runtime-tarball.tar.gz -C "$runtime_unit_root"
tatolabd_executable="$runtime_unit_root/bin/tatolabd"
tatolab_executable="$runtime_unit_root/bin/tatolab"

# Copied with its LICENSE symlink resolved: the workspace is mounted read-only.
cp -rL /workspace/sdk/tatolab-stream /tmp/tatolab-stream-source

"$tatolabd_executable" >/tmp/tatolabd.log 2>&1 &
tatolabd_pid=$!
local_api_deadline=$((SECONDS + 60))
until "$tatolab_executable" streams >/dev/null 2>&1; do
	if ! kill -0 "$tatolabd_pid" 2>/dev/null; then
		cat /tmp/tatolabd.log
		echo "error: tatolabd exited before its local API answered" >&2
		exit 1
	fi
	if [ "$SECONDS" -ge "$local_api_deadline" ]; then
		cat /tmp/tatolabd.log
		echo "error: tatolabd's local API did not answer within 60 s" >&2
		exit 1
	fi
	sleep 1
done

"$tatolab_executable" new /tmp/stream-on-the-software-device --test-pattern
python3 -m venv /tmp/stream-on-the-software-device/.venv
/tmp/stream-on-the-software-device/.venv/bin/pip install --quiet /tmp/tatolab-stream-source 'numpy>=2.1'

cd /tmp/stream-on-the-software-device
"$tatolab_executable" run >/tmp/tatolab-run.log 2>&1 &
tatolab_run_pid=$!
device_deadline=$((SECONDS + 120))
until grep -q 'Selected Vulkan device' /tmp/tatolabd.log; do
	if [ "$SECONDS" -ge "$device_deadline" ] || ! kill -0 "$tatolab_run_pid" 2>/dev/null; then
		break
	fi
	sleep 1
done

tatolabd_survived_the_stream_start=yes
kill -0 "$tatolabd_pid" 2>/dev/null || tatolabd_survived_the_stream_start=no
kill -INT "$tatolab_run_pid" 2>/dev/null || true
wait "$tatolab_run_pid" 2>/dev/null || true
kill -INT "$tatolabd_pid" 2>/dev/null || true
wait "$tatolabd_pid" 2>/dev/null || true

echo "--- tatolab run"
cat /tmp/tatolab-run.log
echo "--- tatolabd"
cat /tmp/tatolabd.log
echo "---"

if [ "$tatolabd_survived_the_stream_start" != yes ]; then
	echo "error: tatolabd exited during the stream start" >&2
	exit 1
fi
if ! grep -q 'Vulkan loader library opened.*_vulkan_driver/libvulkan\.so\.1' /tmp/tatolabd.log; then
	echo "error: tatolabd did not open the Vulkan loader the lend carries" >&2
	exit 1
fi
if ! grep -q 'Selected Vulkan device: llvmpipe' /tmp/tatolabd.log; then
	echo "error: the first stream start built no device on the lavapipe the lend carries" >&2
	exit 1
fi
echo "tatolabd opened the lend's Vulkan loader and built its device on llvmpipe"
