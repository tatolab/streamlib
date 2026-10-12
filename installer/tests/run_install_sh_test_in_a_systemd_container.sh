#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Boots installer/tests/Dockerfile's systemd machine, runs install.sh's test in
# it, and removes it. Needs docker; privileged, because systemd as PID 1 manages
# its own cgroups.

set -euo pipefail

installer_directory="$(cd "$(dirname "$0")/.." && pwd)"
test_machine_image="tatolab-install-sh-test-machine"
test_machine_container="tatolab-install-sh-test-machine-$$"

docker build --quiet --tag "$test_machine_image" "$installer_directory/tests" >/dev/null
docker run --detach --privileged --cgroupns=private --tmpfs /run --tmpfs /run/lock \
	--volume "$installer_directory:/installer:ro" \
	--name "$test_machine_container" "$test_machine_image" >/dev/null
trap 'docker rm --force "$test_machine_container" >/dev/null 2>&1 || true' EXIT

for _ in $(seq 1 100); do
	system_state="$(docker exec "$test_machine_container" systemctl is-system-running 2>/dev/null || true)"
	case "$system_state" in
	running | degraded) break ;;
	esac
	sleep 0.2
done
case "$system_state" in
running | degraded) ;;
*)
	echo "the test machine's systemd never finished booting: ${system_state:-no answer}" >&2
	exit 1
	;;
esac

docker exec "$test_machine_container" \
	bash /installer/tests/install_sh_against_a_fake_release_under_a_user_systemd.sh
