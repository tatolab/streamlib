#!/usr/bin/env bash
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# install.sh against releases this test makes, served over plain http, for a
# user whose home is a fresh directory. Needs curl and python3; no root and no
# service manager, because install.sh registers no service.
#
# A unit's `tatolabd` is a stand-in that records which build is running and
# under which pid, then becomes a `sleep` copied into the build, so the process
# runs an executable from its build folder as a real runtime does.

set -euo pipefail

installer_directory="$(cd "$(dirname "$0")/.." && pwd)"
test_user_home="$(mktemp -d)"
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
# `without-uv` and `without-the-streamlib-commit` leave that member out.
make_runtime_unit() {
	local unit_directory="$1"
	local unit_version="$2"
	local unit_commit="$3"
	local unit_shape="${4:-whole}"
	mkdir -p "$unit_directory/bin" "$unit_directory/lib/tatolab/lend/tatolab/runtime/_vulkan_driver"
	sed -e "s/@UNIT_VERSION@/$unit_version/g" -e "s/@UNIT_COMMIT@/$unit_commit/g" \
		>"$unit_directory/bin/tatolabd" <<'EOF_TATOLABD'
#!/bin/sh
if [ "${1:-}" = --version ]; then
	echo "tatolabd @UNIT_VERSION@"
	exit 0
fi
printf '%s %s %s\n' @UNIT_VERSION@ @UNIT_COMMIT@ "$$" >"$HOME/stand-in-tatolabd-running"
exec "$(dirname "$(readlink -f "$0")")/stand-in-runtime-sleep" infinity
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
	cp "$(readlink -f "$(command -v sleep)")" "$unit_directory/bin/stand-in-runtime-sleep"
	chmod +x "$unit_directory/bin/tatolabd" "$unit_directory/bin/tatolab"
	if [ "$unit_shape" != without-uv ]; then
		printf '#!/bin/sh\necho "uv %s"\n' "$unit_version" >"$unit_directory/bin/uv"
		chmod +x "$unit_directory/bin/uv"
	fi
	touch "$unit_directory/lib/tatolab/lend/tatolab/runtime/__init__.py"
	if [ "$unit_shape" != without-the-bundled-vulkan-icd-manifest ]; then
		printf '{}\n' >"$unit_directory/$bundled_vulkan_icd_manifest_in_a_unit"
	fi
	if [ "$unit_shape" != without-the-streamlib-commit ]; then
		printf '%s\n' "$unit_commit" >"$unit_directory/streamlib-commit"
	fi
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
	env -i \
		HOME="$test_user_home" \
		PATH="$test_user_home/.local/bin:/usr/local/bin:/usr/bin:/bin" \
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

stand_in_runtime_pid=""

start_the_stand_in_runtime() {
	(exec env -i HOME="$test_user_home" PATH="/usr/bin:/bin" "$test_user_home/.local/bin/tatolabd" \
		</dev/null >/dev/null 2>&1) &
	stand_in_runtime_pid=$!
	for _ in $(seq 1 50); do
		[ -e "$test_user_home/stand-in-tatolabd-running" ] && return 0
		sleep 0.1
	done
	fail "the stand-in tatolabd never said it was running"
}

stop_the_stand_in_runtime() {
	[ -n "$stand_in_runtime_pid" ] || return 0
	kill "$stand_in_runtime_pid" 2>/dev/null || true
	wait "$stand_in_runtime_pid" 2>/dev/null || true
	rm -f "$test_user_home/stand-in-tatolabd-running"
	stand_in_runtime_pid=""
}

python3 -m http.server "$release_server_port" --bind 127.0.0.1 --directory "$release_server_root" \
	>/dev/null 2>&1 &
release_server_pid=$!
trap 'stop_the_stand_in_runtime; kill "$release_server_pid" 2>/dev/null || true; rm -rf "$release_server_root" "$test_user_home"' EXIT
release_server_answers=no
for _ in $(seq 1 50); do
	curl --silent --fail "$release_server_base_url/" >/dev/null && release_server_answers=yes && break
	sleep 0.1
done
[ "$release_server_answers" = yes ] || fail "the test's release server never answered on $release_server_base_url"

# --- a first install -------------------------------------------------------

commit_a="$(commit_named a)"
make_release 1.0.0 "$commit_a"
publish_as_latest 1.0.0
first_install_output="$(run_the_installer)"

[ "$(current_build)" = "1.0.0-$commit_a" ] || fail "current does not point at 1.0.0-$commit_a: $(current_build)"
for executable_name in tatolab tatolabd; do
	[ "$(readlink -f "$test_user_home/.local/bin/$executable_name")" = \
		"$(installed_builds_directory)/1.0.0-$commit_a/bin/$executable_name" ] ||
		fail "$test_user_home/.local/bin/$executable_name does not reach the build's bin/"
done
[ ! -e "$test_user_home/.local/bin/uv" ] || fail "install.sh linked the runtime unit's uv into ~/.local/bin"
[ -d "$(installed_builds_directory)/1.0.0-$commit_a/lib/tatolab/lend" ] ||
	fail "lib/ is not a sibling of bin/ in the build"
[ ! -e "$test_user_home/.config/systemd" ] || fail "install.sh wrote a systemd user unit"
[ ! -e "$test_user_home/stand-in-tatolabd-running" ] || fail "install.sh started a runtime"
[ "$(as_the_test_user tatolab)" = "tatolab 1.0.0" ] || fail "tatolab on PATH is not the release's"
case "$first_install_output" in
*"Start the runtime in a terminal of its own with:"*"tatolabd"*) ;;
*) fail "the installer did not say how to start the runtime: $first_install_output" ;;
esac
pass "a first install unpacks the release into its build's folder, links both executables and starts nothing"

# --- an upgrade under a running runtime keeps the build before ---------------

start_the_stand_in_runtime
commit_b="$(commit_named b)"
make_release 1.0.1 "$commit_b"
publish_as_latest 1.0.1
upgrade_output="$(run_the_installer)"
[ "$(current_build)" = "1.0.1-$commit_b" ] || fail "current does not point at 1.0.1-$commit_b after the upgrade"
[ -d "$(installed_builds_directory)/1.0.0-$commit_a" ] || fail "the build before the upgrade was not kept"
[ "$(as_the_test_user tatolab)" = "tatolab 1.0.1" ] || fail "tatolab on PATH is not the upgrade's"
kill -0 "$stand_in_runtime_pid" 2>/dev/null || fail "the upgrade stopped the running runtime"
case "$upgrade_output" in
*"A runtime is running Tatolab 1.0.0-$commit_a; it keeps that build until it is restarted"*) ;;
*) fail "the installer did not say the running runtime keeps its build: $upgrade_output" ;;
esac
pass "an upgrade leaves a running runtime alone, says it keeps its build, and keeps the build before"

# --- a tarball that does not match its sha256 is refused -------------------

make_release 1.0.2 "$(commit_named c)"
echo "0000000000000000000000000000000000000000000000000000000000000000  tatolab-runtime-1.0.2-x86_64-linux.tar.gz" \
	>"$release_server_root/releases/1.0.2/tatolab-runtime-1.0.2-x86_64-linux.tar.gz.sha256"
publish_as_latest 1.0.2
if run_the_installer 2>/dev/null; then
	fail "a tarball not matching its sha256 was installed"
fi
[ "$(current_build)" = "1.0.1-$commit_b" ] || fail "a refused download moved current"
[ -z "$(find "$(installed_builds_directory)" -maxdepth 1 \( -name '1.0.2-*' -o -name '.partial-*' \))" ] ||
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
	[ -z "$(find "$(installed_builds_directory)" -maxdepth 1 \( -name "$release_version-*" -o -name '.partial-*' \))" ] ||
		fail "a tarball without $missing_member was left unpacked"
}
refuses_a_release_shaped without-the-bundled-vulkan-icd-manifest "$bundled_vulkan_icd_manifest_in_a_unit"
pass "a tarball that carries no bundled Vulkan ICD manifest is refused and changes nothing"
refuses_a_release_shaped without-uv bin/uv
pass "a tarball that carries no uv is refused and changes nothing"
refuses_a_release_shaped without-the-streamlib-commit streamlib-commit
pass "a tarball that names no streamlib commit is refused and changes nothing"

# --- a third build keeps one a runtime still runs from ------------------------

commit_d="$(commit_named d)"
make_release 1.0.3 "$commit_d"
publish_as_latest 1.0.3
third_install_output="$(run_the_installer)"
[ "$(current_build)" = "1.0.3-$commit_d" ] || fail "current does not point at 1.0.3-$commit_d"
[ -d "$(installed_builds_directory)/1.0.1-$commit_b" ] || fail "the build before was not kept"
[ -d "$(installed_builds_directory)/1.0.0-$commit_a" ] || fail "the build a runtime runs from was removed"
case "$third_install_output" in
*"Keeping Tatolab 1.0.0-$commit_a, which a running runtime runs from"*) ;;
*) fail "the installer did not say why it kept 1.0.0-$commit_a: $third_install_output" ;;
esac
pass "a build a running runtime runs from is kept past the build before"

# --- --uninstall refuses under a running runtime -------------------------------

if uninstall_refusal="$(run_the_installer --uninstall 2>&1)"; then
	fail "--uninstall removed the build a running runtime runs from"
fi
case "$uninstall_refusal" in
*"a runtime is running Tatolab 1.0.0-$commit_a"*"stop it, then uninstall"*) ;;
*) fail "the --uninstall refusal does not name the running build: $uninstall_refusal" ;;
esac
[ -d "$(installed_builds_directory)/1.0.0-$commit_a" ] || fail "a refused --uninstall removed files"
stop_the_stand_in_runtime
pass "--uninstall refuses while a runtime runs from an installed build, naming it"

# --- a fourth build removes the one two installs back --------------------------

mkdir -p "$(dirname "$test_user_home/.config/systemd/user/tatolabd.service")"
printf '[Service]\nExecStart=%%h/.local/share/tatolab/current/bin/tatolabd\n' \
	>"$test_user_home/.config/systemd/user/tatolabd.service"
commit_e="$(commit_named e)"
make_release 1.0.4 "$commit_e"
publish_as_latest 1.0.4
fourth_install_output="$(run_the_installer)"
[ "$(current_build)" = "1.0.4-$commit_e" ] || fail "current does not point at 1.0.4-$commit_e"
[ -d "$(installed_builds_directory)/1.0.3-$commit_d" ] || fail "the build before was not kept"
for removed_build in "1.0.0-$commit_a" "1.0.1-$commit_b"; do
	[ ! -e "$(installed_builds_directory)/$removed_build" ] || fail "$removed_build was not removed"
done
for expected_step in "Downloading Tatolab 1.0.4" "Installing Tatolab 1.0.4-$commit_e" \
	"Removing Tatolab 1.0.0-$commit_a" "Removing Tatolab 1.0.1-$commit_b" \
	"An earlier install registered $test_user_home/.config/systemd/user/tatolabd.service"; do
	case "$fourth_install_output" in
	*"$expected_step"*) ;;
	*) fail "the installer did not say \"$expected_step\": $fourth_install_output" ;;
	esac
done
[ -e "$test_user_home/.config/systemd/user/tatolabd.service" ] || fail "install.sh removed a unit it no longer owns"
rm -f "$test_user_home/.config/systemd/user/tatolabd.service"
pass "a fourth install keeps the build before, removes the rest, and names an earlier install's unit"

# --- macOS is pointed at Tato Studio -------------------------------------------

macos_uname_directory="$(mktemp -d)"
printf '#!/bin/sh\necho Darwin\n' >"$macos_uname_directory/uname"
chmod +x "$macos_uname_directory/uname"
if macos_refusal="$(as_the_test_user env PATH="$macos_uname_directory:/usr/bin:/bin" \
	sh "$installer_directory/install.sh" 2>&1)"; then
	fail "install.sh did not refuse macOS"
fi
case "$macos_refusal" in
*"Tato Studio"*) ;;
*) fail "the macOS refusal does not point at Tato Studio: $macos_refusal" ;;
esac
rm -rf "$macos_uname_directory"
pass "on macOS the installer refuses, pointing at Tato Studio"

# --- --uninstall leaves the state directory --------------------------------

mkdir -p "$test_user_home/.local/state/tatolab/streams"
touch "$test_user_home/.local/state/tatolab/streams/a-recorded-stream"
run_the_installer --uninstall
[ ! -e "$test_user_home/.local/share/tatolab" ] || fail "--uninstall left the installed releases"
for executable_name in tatolab tatolabd; do
	[ ! -L "$test_user_home/.local/bin/$executable_name" ] || fail "--uninstall left $test_user_home/.local/bin/$executable_name"
done
[ -e "$test_user_home/.local/state/tatolab/streams/a-recorded-stream" ] ||
	fail "--uninstall removed the state directory"
pass "--uninstall removes the files and leaves the state directory"
