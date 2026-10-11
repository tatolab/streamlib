#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# install.sh against releases this test makes, served over plain http, for a
# user whose systemd user manager is real. Run as root inside the container
# installer/tests/Dockerfile boots; run_install_sh_test_in_a_systemd_container.sh
# starts it.
#
# A release's `tatolabd` is a stand-in that records which release is running
# and under which pid, then sleeps; the unit it runs under is the real one.

set -euo pipefail

installer_directory="$(cd "$(dirname "$0")/.." && pwd)"
test_user_name="tatolab-install-test"
release_server_root="$(mktemp -d)"
release_server_port=8731
release_server_base_url="http://127.0.0.1:$release_server_port"

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

pass() {
	echo "ok - $*"
}

make_release() {
	local release_version="$1"
	local release_contents
	release_contents="$(mktemp -d)"
	mkdir -p "$release_contents/bin" "$release_contents/lib/tatolab/lend/tatolab/runtime" \
		"$release_contents/lib/systemd/user"
	cat >"$release_contents/bin/tatolabd" <<EOF
#!/bin/sh
printf '%s %s\n' "$release_version" "\$\$" > "\$HOME/stand-in-tatolabd-running"
exec sleep infinity
EOF
	printf '#!/bin/sh\necho "tatolab %s"\n' "$release_version" >"$release_contents/bin/tatolab"
	chmod +x "$release_contents/bin/tatolabd" "$release_contents/bin/tatolab"
	touch "$release_contents/lib/tatolab/lend/tatolab/runtime/__init__.py"
	cp "$installer_directory/tatolabd.service" \
		"$installer_directory/tatolabd-restart-for-a-display-at-login.service" \
		"$release_contents/lib/systemd/user/"

	local release_directory="$release_server_root/releases/$release_version"
	local tarball_name="tatolab-runtime-$release_version-x86_64-linux.tar.gz"
	mkdir -p "$release_directory"
	tar -C "$release_contents" -czf "$release_directory/$tarball_name" bin lib
	(cd "$release_directory" && sha256sum "$tarball_name" >"$tarball_name.sha256")
	rm -rf "$release_contents"
}

publish_as_latest() {
	printf '%s\n' "$1" >"$release_server_root/latest"
}

as_the_test_user() {
	runuser -u "$test_user_name" -- env \
		HOME="$test_user_home" \
		PATH="$test_user_home/.local/bin:/usr/local/bin:/usr/bin:/bin" \
		XDG_RUNTIME_DIR="/run/user/$test_user_uid" \
		DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$test_user_uid/bus" \
		TATOLAB_INSTALL_BASE_URL="$release_server_base_url" \
		"$@"
}

run_the_installer() {
	as_the_test_user sh "$installer_directory/install.sh" "$@" </dev/null
}

running_release_and_pid() {
	cat "$test_user_home/stand-in-tatolabd-running" 2>/dev/null || true
}

the_service_main_pid() {
	as_the_test_user systemctl --user show tatolabd.service --property=MainPID --value
}

wait_until_the_running_release_is() {
	local expected_release_version="$1"
	for _ in $(seq 1 100); do
		case "$(running_release_and_pid)" in
		"$expected_release_version "*) return 0 ;;
		esac
		sleep 0.1
	done
	fail "release $expected_release_version never started; running: $(running_release_and_pid)"
}

# --- the machine: a user with a running user manager ------------------------

useradd --create-home "$test_user_name"
test_user_uid="$(id -u "$test_user_name")"
test_user_home="$(getent passwd "$test_user_name" | cut -d: -f6)"
loginctl enable-linger "$test_user_name"
for _ in $(seq 1 100); do
	[ -S "/run/user/$test_user_uid/bus" ] && as_the_test_user systemctl --user is-system-running --wait >/dev/null 2>&1 && break
	sleep 0.2
done
as_the_test_user systemctl --user show-environment >/dev/null || fail "the test user's systemd user manager never answered"

python3 -m http.server "$release_server_port" --bind 127.0.0.1 --directory "$release_server_root" \
	>/dev/null 2>&1 &
release_server_pid=$!
trap 'kill "$release_server_pid" 2>/dev/null || true' EXIT
for _ in $(seq 1 50); do
	curl --silent --fail "$release_server_base_url/" >/dev/null && break
	sleep 0.1
done

# --- a first install -------------------------------------------------------

make_release 1.0.0
publish_as_latest 1.0.0
run_the_installer
wait_until_the_running_release_is 1.0.0

[ "$(readlink "$test_user_home/.local/share/tatolab/current")" = "1.0.0" ] ||
	fail "current does not point at 1.0.0"
for executable_name in tatolab tatolabd; do
	[ "$(readlink -f "$test_user_home/.local/bin/$executable_name")" = \
		"$test_user_home/.local/share/tatolab/1.0.0/bin/$executable_name" ] ||
		fail "$test_user_home/.local/bin/$executable_name does not reach the release's bin/"
done
[ -d "$test_user_home/.local/share/tatolab/1.0.0/lib/tatolab/lend" ] ||
	fail "lib/ is not a sibling of bin/ in the release"
for unit_name in tatolabd.service tatolabd-restart-for-a-display-at-login.service; do
	cmp -s "$installer_directory/$unit_name" "$test_user_home/.config/systemd/user/$unit_name" ||
		fail "the $unit_name written is not the release's"
	[ "$(as_the_test_user systemctl --user is-enabled "$unit_name")" = "enabled" ] ||
		fail "$unit_name is not enabled"
done
[ "$(as_the_test_user systemctl --user is-active tatolabd.service)" = "active" ] ||
	fail "tatolabd.service is not active"
[ "$(as_the_test_user tatolab)" = "tatolab 1.0.0" ] || fail "tatolab on PATH is not the release's"
pass "a first install unpacks the release, links both executables and starts the service"

# --- kill -9 is restarted --------------------------------------------------

killed_main_pid="$(the_service_main_pid)"
kill -9 "$killed_main_pid"
for _ in $(seq 1 100); do
	restarted_main_pid="$(the_service_main_pid)"
	[ "$restarted_main_pid" != "0" ] && [ "$restarted_main_pid" != "$killed_main_pid" ] && break
	sleep 0.1
done
if [ "$restarted_main_pid" = "0" ] || [ "$restarted_main_pid" = "$killed_main_pid" ]; then
	fail "tatolabd was not restarted within 10 s of kill -9"
fi
[ "$(as_the_test_user systemctl --user is-active tatolabd.service)" = "active" ] ||
	fail "tatolabd.service is not active after its restart"
pass "a tatolabd killed with -9 is restarted within 10 s"

# --- a login restarts a runtime that started with no display, and only that one

no_display_marker="/run/user/$test_user_uid/streamlib/tatolabd-started-with-no-display-server"
pid_before_a_login="$(the_service_main_pid)"
as_the_test_user systemctl --user start tatolabd-restart-for-a-display-at-login.service
[ "$(the_service_main_pid)" = "$pid_before_a_login" ] ||
	fail "a login restarted a runtime that had started with a display"
as_the_test_user mkdir -p "$(dirname "$no_display_marker")"
as_the_test_user touch "$no_display_marker"
as_the_test_user systemctl --user start tatolabd-restart-for-a-display-at-login.service
for _ in $(seq 1 100); do
	[ "$(the_service_main_pid)" != "$pid_before_a_login" ] && [ "$(the_service_main_pid)" != "0" ] && break
	sleep 0.1
done
[ "$(the_service_main_pid)" != "$pid_before_a_login" ] ||
	fail "a login did not restart a runtime that had started with no display"
as_the_test_user rm -f "$no_display_marker"
pass "a login restarts the runtime only when it started with no display server"

# --- re-running upgrades in place and restarts -----------------------------

pid_before_the_upgrade="$(the_service_main_pid)"
make_release 1.0.1
publish_as_latest 1.0.1
run_the_installer
wait_until_the_running_release_is 1.0.1
[ "$(readlink "$test_user_home/.local/share/tatolab/current")" = "1.0.1" ] ||
	fail "current does not point at 1.0.1 after the upgrade"
[ ! -e "$test_user_home/.local/share/tatolab/1.0.0" ] || fail "the replaced release 1.0.0 was left behind"
[ "$(the_service_main_pid)" != "$pid_before_the_upgrade" ] || fail "the upgrade did not restart the service"
[ "$(as_the_test_user tatolab)" = "tatolab 1.0.1" ] || fail "tatolab on PATH is not the upgrade's"
pass "re-running the installer upgrades in place and restarts the service"

# --- a tarball that does not match its sha256 is refused -------------------

make_release 1.0.2
echo "0000000000000000000000000000000000000000000000000000000000000000  tatolab-runtime-1.0.2-x86_64-linux.tar.gz" \
	>"$release_server_root/releases/1.0.2/tatolab-runtime-1.0.2-x86_64-linux.tar.gz.sha256"
publish_as_latest 1.0.2
if run_the_installer 2>/dev/null; then
	fail "a tarball not matching its sha256 was installed"
fi
[ "$(readlink "$test_user_home/.local/share/tatolab/current")" = "1.0.1" ] ||
	fail "a refused download moved current"
[ ! -e "$test_user_home/.local/share/tatolab/1.0.2" ] || fail "a refused download was unpacked"
pass "a tarball that does not match its published sha256 is refused and changes nothing"

# --- a service that fails to restart keeps the release before it ----------

make_release 1.0.3
publish_as_latest 1.0.3
failing_restart_directory="$(mktemp -d)"
cat >"$failing_restart_directory/systemctl" <<'EOF_SYSTEMCTL'
#!/bin/sh
for argument in "$@"; do
	[ "$argument" = restart ] && exit 1
done
exec /usr/bin/systemctl "$@"
EOF_SYSTEMCTL
chmod 755 "$failing_restart_directory" "$failing_restart_directory/systemctl"
if as_the_test_user env PATH="$failing_restart_directory:/usr/bin:/bin" \
	sh "$installer_directory/install.sh" </dev/null 2>/dev/null; then
	fail "an install whose service did not restart reported success"
fi
[ -d "$test_user_home/.local/share/tatolab/1.0.1" ] ||
	fail "an install whose service did not restart removed the release before it"
run_the_installer
wait_until_the_running_release_is 1.0.3
[ ! -e "$test_user_home/.local/share/tatolab/1.0.1" ] || fail "the next good install left 1.0.1 behind"
pass "a service that fails to restart fails the install and keeps the release before it"

# --- macOS is pointed at the app -------------------------------------------

macos_uname_directory="$(mktemp -d)"
printf '#!/bin/sh\necho Darwin\n' >"$macos_uname_directory/uname"
chmod +x "$macos_uname_directory/uname"
chmod 755 "$macos_uname_directory"
if macos_refusal="$(as_the_test_user env PATH="$macos_uname_directory:/usr/bin:/bin" \
	sh "$installer_directory/install.sh" 2>&1)"; then
	fail "install.sh did not refuse macOS"
fi
case "$macos_refusal" in
*"Tatolab app"*"from source"*) ;;
*) fail "the macOS refusal does not point at the app: $macos_refusal" ;;
esac
pass "on macOS the installer refuses, pointing at the app"

# --- --uninstall leaves the state directory --------------------------------

as_the_test_user mkdir -p "$test_user_home/.local/state/tatolab/streams"
as_the_test_user touch "$test_user_home/.local/state/tatolab/streams/a-kept-stream"
run_the_installer --uninstall
[ "$(as_the_test_user systemctl --user is-active tatolabd.service || true)" != "active" ] ||
	fail "tatolabd.service still runs after --uninstall"
for unit_name in tatolabd.service tatolabd-restart-for-a-display-at-login.service; do
	[ ! -e "$test_user_home/.config/systemd/user/$unit_name" ] || fail "--uninstall left $unit_name"
done
[ ! -e "$test_user_home/.local/share/tatolab" ] || fail "--uninstall left the installed releases"
for executable_name in tatolab tatolabd; do
	[ ! -L "$test_user_home/.local/bin/$executable_name" ] || fail "--uninstall left $test_user_home/.local/bin/$executable_name"
done
[ -e "$test_user_home/.local/state/tatolab/streams/a-kept-stream" ] ||
	fail "--uninstall removed the state directory"
pass "--uninstall removes the service and the files and leaves the state directory"
