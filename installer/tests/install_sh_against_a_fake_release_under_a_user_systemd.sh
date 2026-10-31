#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# install.sh against releases this test makes, served over plain http, and
# against runtime units it hands `--from-unit`, for a user whose systemd user
# manager is real. Run as root inside the container installer/tests/Dockerfile
# boots; run_install_sh_test_in_a_systemd_container.sh starts it.
#
# A unit's `tatolabd` is a stand-in that records which build is running and
# under which pid, then sleeps; its `tatolab --version` answers for that build
# while it runs, as the real one asks the runtime. The unit it runs under is the
# real one.

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

bundled_vulkan_icd_manifest_in_a_unit="lib/tatolab/lend/tatolab/runtime/_vulkan_driver/lvp_icd.x86_64.json"

# The 40-hex streamlib commit a test names by `$1`.
commit_named() {
	printf '%s' "$1" | sha1sum | cut -c 1-40
}

# A runtime unit of version `$2` built from commit `$3`, written to `$1`. A
# fourth argument shapes it: `without-the-bundled-vulkan-icd-manifest`,
# `without-uv` and `without-the-streamlib-commit` leave that member out, and
# `tatolabd-exits-at-start` makes its runtime exit failed whenever it starts.
make_runtime_unit() {
	local unit_directory="$1"
	local unit_version="$2"
	local unit_commit="$3"
	local unit_shape="${4:-whole}"
	mkdir -p "$unit_directory/bin" "$unit_directory/lib/tatolab/lend/tatolab/runtime/_vulkan_driver" \
		"$unit_directory/lib/tatolab/watch" "$unit_directory/lib/systemd/user"
	local exits_at_start=no
	[ "$unit_shape" = tatolabd-exits-at-start ] && exits_at_start=yes
	sed -e "s/@UNIT_VERSION@/$unit_version/g" -e "s/@UNIT_COMMIT@/$unit_commit/g" \
		-e "s/@EXITS_AT_START@/$exits_at_start/g" >"$unit_directory/bin/tatolabd" <<'EOF_TATOLABD'
#!/bin/sh
if [ "${1:-}" = --version ]; then
	echo "tatolabd @UNIT_VERSION@"
	exit 0
fi
[ @EXITS_AT_START@ = yes ] && exit 1
running_file="$HOME/stand-in-tatolabd-running"
trap 'rm -f "$running_file"; exit 0' TERM
printf '%s %s %s\n' @UNIT_VERSION@ @UNIT_COMMIT@ "$$" >"$running_file"
sleep infinity &
wait
EOF_TATOLABD
	sed -e "s/@UNIT_VERSION@/$unit_version/g" >"$unit_directory/bin/tatolab" <<'EOF_TATOLAB'
#!/bin/sh
echo "tatolab @UNIT_VERSION@"
[ "${1:-}" = --version ] || exit 0
if read -r running_version running_commit running_pid 2>/dev/null <"$HOME/stand-in-tatolabd-running" &&
	kill -0 "$running_pid" 2>/dev/null; then
	echo "runtime $running_version ($running_version+$running_commit.stand-in), up 0d 0h 0m, keeps no own log"
else
	echo "runtime: not running"
fi
EOF_TATOLAB
	printf '#!/bin/sh\nexec sleep 3600\n' >"$unit_directory/bin/moq-relay"
	chmod +x "$unit_directory/bin/tatolabd" "$unit_directory/bin/tatolab" "$unit_directory/bin/moq-relay"
	if [ "$unit_shape" != without-uv ]; then
		printf '#!/bin/sh\necho "uv %s"\n' "$unit_version" >"$unit_directory/bin/uv"
		chmod +x "$unit_directory/bin/uv"
	fi
	touch "$unit_directory/lib/tatolab/lend/tatolab/runtime/__init__.py"
	printf '<!doctype html>\n' >"$unit_directory/lib/tatolab/watch/index.html"
	if [ "$unit_shape" != without-the-bundled-vulkan-icd-manifest ]; then
		printf '{}\n' >"$unit_directory/$bundled_vulkan_icd_manifest_in_a_unit"
	fi
	if [ "$unit_shape" != without-the-streamlib-commit ]; then
		printf '%s\n' "$unit_commit" >"$unit_directory/streamlib-commit"
	fi
	cp "$installer_directory/tatolabd.service" "$unit_directory/lib/systemd/user/"
	chmod -R a+rX "$unit_directory"
}

# A release of version `$1` from commit `$2`, shaped as make_runtime_unit's `$3`.
make_release() {
	local release_version="$1"
	local release_commit="$2"
	local release_contents
	release_contents="$(mktemp -d)"
	make_runtime_unit "$release_contents" "$release_version" "$release_commit" "${3:-whole}"
	local release_directory="$release_server_root/releases/$release_version"
	local tarball_name="tatolab-runtime-$release_version-x86_64-linux.tar.gz"
	mkdir -p "$release_directory"
	local release_members=(bin lib)
	[ -e "$release_contents/streamlib-commit" ] && release_members+=(streamlib-commit)
	tar -C "$release_contents" -czf "$release_directory/$tarball_name" "${release_members[@]}"
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

installed_builds_directory() {
	printf '%s' "$test_user_home/.local/share/tatolab"
}

current_build() {
	readlink "$(installed_builds_directory)/current" || true
}

running_build_and_pid() {
	cat "$test_user_home/stand-in-tatolabd-running" 2>/dev/null || true
}

the_service_main_pid() {
	as_the_test_user systemctl --user show tatolabd.service --property=MainPID --value
}

# The installer returns once the runtime answers, so the build is running by then.
expect_the_running_build_to_be() {
	local expected_version="$1"
	local expected_commit="$2"
	case "$(running_build_and_pid)" in
	"$expected_version $expected_commit "*) ;;
	*) fail "$expected_version at $expected_commit is not running; running: $(running_build_and_pid)" ;;
	esac
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

commit_a="$(commit_named a)"
make_release 1.0.0 "$commit_a"
publish_as_latest 1.0.0
run_the_installer
expect_the_running_build_to_be 1.0.0 "$commit_a"

[ "$(current_build)" = "1.0.0-$commit_a" ] || fail "current does not point at 1.0.0-$commit_a: $(current_build)"
for executable_name in tatolab tatolabd; do
	[ "$(readlink -f "$test_user_home/.local/bin/$executable_name")" = \
		"$(installed_builds_directory)/1.0.0-$commit_a/bin/$executable_name" ] ||
		fail "$test_user_home/.local/bin/$executable_name does not reach the build's bin/"
done
[ ! -e "$test_user_home/.local/bin/uv" ] || fail "install.sh linked the runtime unit's uv into ~/.local/bin"
[ -d "$(installed_builds_directory)/1.0.0-$commit_a/lib/tatolab/lend" ] ||
	fail "lib/ is not a sibling of bin/ in the build"
cmp -s "$installer_directory/tatolabd.service" "$test_user_home/.config/systemd/user/tatolabd.service" ||
	fail "the tatolabd.service written is not the release's"
[ "$(as_the_test_user systemctl --user is-enabled tatolabd.service)" = "enabled" ] ||
	fail "tatolabd.service is not enabled"
[ "$(as_the_test_user systemctl --user is-active tatolabd.service)" = "active" ] ||
	fail "tatolabd.service is not active"
[ "$(as_the_test_user tatolab)" = "tatolab 1.0.0" ] || fail "tatolab on PATH is not the release's"
pass "a first install unpacks the release into its build's folder, links both executables and starts the service"

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

# --- re-running upgrades and keeps the build before ------------------------

commit_b="$(commit_named b)"
pid_before_the_upgrade="$(the_service_main_pid)"
make_release 1.0.1 "$commit_b"
publish_as_latest 1.0.1
run_the_installer
expect_the_running_build_to_be 1.0.1 "$commit_b"
[ "$(current_build)" = "1.0.1-$commit_b" ] || fail "current does not point at 1.0.1-$commit_b after the upgrade"
[ -d "$(installed_builds_directory)/1.0.0-$commit_a" ] || fail "the build before the upgrade was not kept"
[ "$(the_service_main_pid)" != "$pid_before_the_upgrade" ] || fail "the upgrade did not restart the service"
[ "$(as_the_test_user tatolab)" = "tatolab 1.0.1" ] || fail "tatolab on PATH is not the upgrade's"
pass "re-running the installer upgrades, restarts the service and keeps the build before"

# --- a tarball that does not match its sha256 is refused -------------------

make_release 1.0.2 "$(commit_named c)"
echo "0000000000000000000000000000000000000000000000000000000000000000  tatolab-runtime-1.0.2-x86_64-linux.tar.gz" \
	>"$release_server_root/releases/1.0.2/tatolab-runtime-1.0.2-x86_64-linux.tar.gz.sha256"
publish_as_latest 1.0.2
if run_the_installer 2>/dev/null; then
	fail "a tarball not matching its sha256 was installed"
fi
[ "$(current_build)" = "1.0.1-$commit_b" ] || fail "a refused download moved current"
[ -z "$(find "$(installed_builds_directory)" -maxdepth 1 -name '1.0.2-*' -o -maxdepth 1 -name '.partial-*')" ] ||
	fail "a refused download was left unpacked"
pass "a tarball that does not match its published sha256 is refused and changes nothing"

# --- a unit missing a member is refused, naming it ---------------------------

refuses_a_release_shaped() {
	local release_shape="$1"
	local missing_member="$2"
	local release_version="1.0.2-$release_shape"
	make_release "$release_version" "$(commit_named "$release_shape")" "$release_shape"
	publish_as_latest "$release_version"
	local refusal
	if refusal="$(run_the_installer 2>&1)"; then
		fail "a tarball carrying no $missing_member was installed"
	fi
	case "$refusal" in
	*"carries no $missing_member"*) ;;
	*) fail "the refusal does not name the missing $missing_member: $refusal" ;;
	esac
	[ "$(current_build)" = "1.0.1-$commit_b" ] || fail "a tarball without $missing_member moved current"
	[ -z "$(find "$(installed_builds_directory)" -maxdepth 1 -name "$release_version-*" -o -maxdepth 1 -name '.partial-*')" ] ||
		fail "a tarball without $missing_member was left unpacked"
}
refuses_a_release_shaped without-the-bundled-vulkan-icd-manifest "$bundled_vulkan_icd_manifest_in_a_unit"
pass "a tarball that carries no bundled Vulkan ICD manifest is refused and changes nothing"
refuses_a_release_shaped without-uv bin/uv
pass "a tarball that carries no uv is refused and changes nothing"
refuses_a_release_shaped without-the-streamlib-commit streamlib-commit
pass "a tarball that names no streamlib commit is refused and changes nothing"

# --- a service that fails to start keeps the build before it ---------------

commit_d="$(commit_named d)"
make_release 1.0.3 "$commit_d"
publish_as_latest 1.0.3
failing_start_directory="$(mktemp -d)"
cat >"$failing_start_directory/systemctl" <<'EOF_SYSTEMCTL'
#!/bin/sh
for argument in "$@"; do
	[ "$argument" = start ] && exit 1
done
exec /usr/bin/systemctl "$@"
EOF_SYSTEMCTL
chmod 755 "$failing_start_directory" "$failing_start_directory/systemctl"
if as_the_test_user env PATH="$failing_start_directory:/usr/bin:/bin" \
	sh "$installer_directory/install.sh" </dev/null 2>/dev/null; then
	fail "an install whose service did not start reported success"
fi
[ "$(current_build)" = "1.0.1-$commit_b" ] || fail "an install whose service did not start left current on the new build"
[ "$(as_the_test_user tatolab)" = "tatolab 1.0.1" ] ||
	fail "an install whose service did not start left tatolab on PATH at the new build"
[ ! -e "$(installed_builds_directory)/1.0.3-$commit_d" ] || fail "the build whose service did not start was kept"
pass "a service that fails to start fails the install and puts the build before it back"

# --- --from-unit: two units of one version at different commits -------------

units_root="$(mktemp -d)"
chmod 755 "$units_root"
commit_e="$(commit_named e)"
commit_f="$(commit_named f)"
make_runtime_unit "$units_root/dev-e" 1.0.4 "$commit_e"
make_runtime_unit "$units_root/dev-f" 1.0.4 "$commit_f"
run_the_installer --from-unit "$units_root/dev-e"
expect_the_running_build_to_be 1.0.4 "$commit_e"
[ ! -e "$(installed_builds_directory)/1.0.0-$commit_a" ] || fail "the build two installs back was not removed"
from_unit_output="$(run_the_installer --from-unit "$units_root/dev-f")"
expect_the_running_build_to_be 1.0.4 "$commit_f"
[ "$(current_build)" = "1.0.4-$commit_f" ] || fail "current does not point at the second unit's build"
[ -d "$(installed_builds_directory)/1.0.4-$commit_e" ] || fail "the first unit's build was not kept as the build before"
[ ! -e "$(installed_builds_directory)/1.0.1-$commit_b" ] || fail "the build two installs back was not removed"
for expected_step in "Copying the runtime unit from $units_root/dev-f" "Stopping the runtime" \
	"Starting Tatolab 1.0.4-$commit_f" "Waiting for Tatolab 1.0.4-$commit_f to answer" \
	"Tatolab 1.0.4-$commit_f answers" "Removing Tatolab 1.0.1-$commit_b"; do
	case "$from_unit_output" in
	*"$expected_step"*) ;;
	*) fail "the installer did not say \"$expected_step\": $from_unit_output" ;;
	esac
done
pass "two units of one version at different commits leave the second running and the first kept"

# --- --from-unit: a unit whose tatolabd exits at start ----------------------

commit_g="$(commit_named g)"
make_runtime_unit "$units_root/exits" 1.0.5 "$commit_g" tatolabd-exits-at-start
if exiting_unit_refusal="$(run_the_installer --from-unit "$units_root/exits" 2>&1)"; then
	fail "a unit whose tatolabd exits at start was installed"
fi
case "$exiting_unit_refusal" in
*"1.0.5-$commit_g's service did not start, so 1.0.4-$commit_f is back in place"*) ;;
*) fail "the refusal does not name the build put back: $exiting_unit_refusal" ;;
esac
for _ in $(seq 1 100); do
	case "$(running_build_and_pid)" in "1.0.4 $commit_f "*) break ;; esac
	sleep 0.1
done
expect_the_running_build_to_be 1.0.4 "$commit_f"
[ "$(current_build)" = "1.0.4-$commit_f" ] || fail "an exiting unit left current on its build"
[ ! -e "$(installed_builds_directory)/1.0.5-$commit_g" ] || fail "the exiting unit's build was kept"
pass "a unit whose tatolabd exits at start leaves the build before it running, and says which"

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
as_the_test_user touch "$test_user_home/.local/state/tatolab/streams/a-recorded-stream"
run_the_installer --uninstall
[ "$(as_the_test_user systemctl --user is-active tatolabd.service || true)" != "active" ] ||
	fail "tatolabd.service still runs after --uninstall"
[ ! -e "$test_user_home/.config/systemd/user/tatolabd.service" ] || fail "--uninstall left tatolabd.service"
[ ! -e "$test_user_home/.local/share/tatolab" ] || fail "--uninstall left the installed releases"
for executable_name in tatolab tatolabd; do
	[ ! -L "$test_user_home/.local/bin/$executable_name" ] || fail "--uninstall left $test_user_home/.local/bin/$executable_name"
done
[ -e "$test_user_home/.local/state/tatolab/streams/a-recorded-stream" ] ||
	fail "--uninstall removed the state directory"
pass "--uninstall removes the service and the files and leaves the state directory"
