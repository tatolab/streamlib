#!/bin/sh
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Installs the Tatolab runtime on a Linux machine without Tato Studio, which
# runs the runtime it carries itself:
#
#   curl -fsSL https://get.tatolab.com/install.sh | sh
#
# Each build is installed into a folder of its own, <version>-<commit>, and
# `current` names the newest; `tatolab` and `tatolabd` in ~/.local/bin link
# through `current`. It registers no service and starts no runtime: run
# `tatolabd` yourself, or under a service manager of your own. A runtime
# already running keeps its build until it is restarted. The build before is
# kept and older ones are removed. Uninstall with
#
#   curl -fsSL https://get.tatolab.com/install.sh | sh -s -- --uninstall
#
# which removes the installed files and never the state directory, where the
# runtime keeps its streams and logs.
#
# TATOLAB_INSTALL_VERSION installs that release rather than the newest one.
# TATOLAB_INSTALL_BASE_URL serves the releases from somewhere else, which is
# how the installer's own tests run it against a release they made.

set -eu

TATOLAB_INSTALL_BASE_URL="${TATOLAB_INSTALL_BASE_URL:-https://get.tatolab.com}"

TATOLAB_EXECUTABLE_LINK_DIRECTORY="$HOME/.local/bin"
STREAMLIB_COMMIT_FILE_NAME="streamlib-commit"

TATOLAB_INSTALL_DIRECTORY="$HOME/.local/share/tatolab"
TATOLAB_STATE_DIRECTORY="${XDG_STATE_HOME:-$HOME/.local/state}/tatolab"

say() {
	printf '%s\n' "$*"
}

warn() {
	printf 'tatolab install: warning: %s\n' "$*" >&2
}

refuse() {
	printf 'tatolab install: %s\n' "$*" >&2
	exit 1
}

usage_line() {
	say "usage: install.sh [--uninstall]"
}

require_command() {
	command -v "$1" >/dev/null 2>&1 || refuse "needs \`$1\`, which is not on PATH"
}

sha256_of_file() {
	if command -v sha256sum >/dev/null 2>&1; then
		sha256sum "$1" | cut -d ' ' -f 1
	elif command -v shasum >/dev/null 2>&1; then
		shasum -a 256 "$1" | cut -d ' ' -f 1
	else
		refuse "needs \`sha256sum\` or \`shasum\` to check the download, and neither is on PATH"
	fi
}

# Never lets a redirect off https leave it; a plain-http base URL is a test's.
download() {
	case "$TATOLAB_INSTALL_BASE_URL" in
	https://*) download_protocols='=https' ;;
	*) download_protocols='=http,https' ;;
	esac
	curl --fail --silent --show-error --location --proto "$download_protocols" \
		--proto-redir "$download_protocols" --retry 3 \
		--output "$2" "$1" || refuse "could not download $1"
}

is_a_version() {
	case "$1" in
	'' | *[!0-9A-Za-z.+-]* | .* | -*) return 1 ;;
	esac
}

is_a_full_commit() {
	case "$1" in
	*[!0-9a-f]*) return 1 ;;
	esac
	[ "${#1}" -eq 40 ]
}

# --- this machine -------------------------------------------------------------

refuse_a_machine_with_no_runtime_build() {
	operating_system="$(uname -s)"
	machine_architecture="$(uname -m)"
	case "$operating_system" in
	Linux)
		case "$machine_architecture" in
		x86_64 | amd64) ;;
		*) refuse "there is no Tatolab runtime for $machine_architecture Linux yet; x86_64 is the only Linux build" ;;
		esac
		;;
	Darwin) refuse "on macOS, the runtime comes with Tato Studio, which runs it while the app is open" ;;
	*) refuse "there is no Tatolab runtime for $operating_system" ;;
	esac
}

# What a runtime unit must carry, one per line.
required_members_of_a_unit() {
	printf '%s\n' bin/tatolabd bin/tatolab bin/moq-relay bin/uv lib/tatolab/lend \
		lib/tatolab/lend/tatolab/runtime/_vulkan_driver/lvp_icd.x86_64.json \
		lib/tatolab/watch/index.html \
		"$STREAMLIB_COMMIT_FILE_NAME"
}

# The executables linked into ~/.local/bin, as `<link name> <path in a build>` lines.
executable_links_into_a_build() {
	printf '%s\n' "tatolab bin/tatolab" "tatolabd bin/tatolabd"
}

# The runtime line `tatolab --version` prints for the runtime answering on this
# user's local API, or nothing when none answers.
running_runtime_line() {
	"$TATOLAB_INSTALL_DIRECTORY/current/bin/tatolab" --version 2>/dev/null | grep '^runtime [0-9]' || true
}

# --- the build ---------------------------------------------------------------

# The build a staged runtime unit is, `<version>-<commit>`: its runtime's
# version and the streamlib commit it names.
build_of_the_staged_unit() {
	staged_unit_directory="$1"
	runtime_version_line="$("$staged_unit_directory/bin/tatolabd" --version 2>&1)" ||
		refuse "the runtime unit's tatolabd does not run here: $runtime_version_line"
	unit_version="${runtime_version_line#tatolabd }"
	is_a_version "$unit_version" || refuse "the runtime unit's tatolabd names no version: $runtime_version_line"
	unit_commit="$(tr -d ' \t\r\n' <"$staged_unit_directory/$STREAMLIB_COMMIT_FILE_NAME")"
	is_a_full_commit "$unit_commit" ||
		refuse "the runtime unit's $STREAMLIB_COMMIT_FILE_NAME names no full commit: \`$unit_commit\`"
	printf '%s-%s' "$unit_version" "$unit_commit"
}

first_missing_member_of() {
	required_members_of_a_unit | while IFS= read -r required_member; do
		[ -e "$1/$required_member" ] || {
			printf '%s' "$required_member"
			break
		}
	done
}

release_version_to_install() {
	if [ -n "${TATOLAB_INSTALL_VERSION:-}" ]; then
		release_version="${TATOLAB_INSTALL_VERSION#v}"
	else
		download "$TATOLAB_INSTALL_BASE_URL/latest" "$download_directory/latest"
		release_version="$(tr -d ' \t\r\n' <"$download_directory/latest")"
	fi
	is_a_version "$release_version" || refuse "\`$release_version\` is not a release version"
	printf '%s' "$release_version"
}

download_the_release_into() {
	release_version="$(release_version_to_install)"
	tarball_name="tatolab-runtime-$release_version-x86_64-linux.tar.gz"
	tarball_url="$TATOLAB_INSTALL_BASE_URL/releases/$release_version/$tarball_name"
	say "Downloading Tatolab $release_version"
	download "$tarball_url" "$download_directory/$tarball_name"
	download "$tarball_url.sha256" "$download_directory/$tarball_name.sha256"
	expected_sha256="$(cut -d ' ' -f 1 <"$download_directory/$tarball_name.sha256")"
	actual_sha256="$(sha256_of_file "$download_directory/$tarball_name")"
	[ "$actual_sha256" = "$expected_sha256" ] ||
		refuse "$tarball_name does not match its published sha256 (got $actual_sha256, expected $expected_sha256)"
	tar -xzf "$download_directory/$tarball_name" -C "$1"
	unit_description="$tarball_name"
}

# Stages the unit, checks it, and leaves it in its build's folder, named in `build`.
stage_the_unit_into_its_build_folder() {
	mkdir -p "$TATOLAB_INSTALL_DIRECTORY"
	partial_build_directory="$TATOLAB_INSTALL_DIRECTORY/.partial-$$"
	rm -rf "$partial_build_directory"
	mkdir "$partial_build_directory"
	download_the_release_into "$partial_build_directory"
	missing_member="$(first_missing_member_of "$partial_build_directory")"
	[ -z "$missing_member" ] || refuse "$unit_description carries no $missing_member"
	build="$(build_of_the_staged_unit "$partial_build_directory")" || exit 1
	build_directory="$TATOLAB_INSTALL_DIRECTORY/$build"
	if [ -d "$build_directory" ] && [ "$(readlink "$TATOLAB_INSTALL_DIRECTORY/current" 2>/dev/null || true)" = "$build" ]; then
		say "Tatolab $build is already installed in $build_directory"
		rm -rf "$partial_build_directory"
	else
		say "Installing Tatolab $build into $build_directory"
		rm -rf "$build_directory"
		mv "$partial_build_directory" "$build_directory"
	fi
}

point_current_at() {
	ln -sfn "$1" "$TATOLAB_INSTALL_DIRECTORY/current"
	mkdir -p "$TATOLAB_EXECUTABLE_LINK_DIRECTORY"
	executable_links_into_a_build | while read -r link_name path_in_a_build; do
		ln -sfn "$TATOLAB_INSTALL_DIRECTORY/current/$path_in_a_build" "$TATOLAB_EXECUTABLE_LINK_DIRECTORY/$link_name"
	done
}

remove_every_build_but() {
	for installed_build_directory in "$TATOLAB_INSTALL_DIRECTORY"/*; do
		[ -d "$installed_build_directory" ] || continue
		[ -L "$installed_build_directory" ] && continue
		installed_build="$(basename "$installed_build_directory")"
		[ "$installed_build" = "$1" ] && continue
		[ "$installed_build" = "$2" ] && continue
		say "Removing Tatolab $installed_build"
		rm -rf "$installed_build_directory"
	done
}

say_where_the_executables_are() {
	case ":$PATH:" in
	*":$TATOLAB_EXECUTABLE_LINK_DIRECTORY:"*) ;;
	*) warn "$TATOLAB_EXECUTABLE_LINK_DIRECTORY is not on your PATH; add it to run \`tatolab\`" ;;
	esac
}

# Says which build a runtime already running keeps until it is restarted.
say_what_a_running_runtime_keeps() {
	runtime_line="$(running_runtime_line)"
	[ -n "$runtime_line" ] || return 0
	case "$runtime_line" in
	"runtime $1 ("*) return 0 ;;
	esac
	say "A runtime is running ($runtime_line); it keeps that build until it is restarted, and" \
		"starts as $1 the next time."
}

install_tatolab() {
	refuse_a_machine_with_no_runtime_build
	require_command curl
	require_command tar
	download_directory="$(mktemp -d)"
	trap 'rm -rf "$download_directory" "$TATOLAB_INSTALL_DIRECTORY/.partial-$$"' EXIT

	stage_the_unit_into_its_build_folder
	build_version="${build%-*}"
	previous_build="$(readlink "$TATOLAB_INSTALL_DIRECTORY/current" 2>/dev/null || true)"
	if [ -n "$previous_build" ] && [ "$previous_build" != "$build" ]; then
		ln -sfn "$previous_build" "$TATOLAB_INSTALL_DIRECTORY/previous"
	fi
	point_current_at "$build"
	remove_every_build_but "$build" "$(readlink "$TATOLAB_INSTALL_DIRECTORY/previous" 2>/dev/null || true)"
	say_where_the_executables_are
	say_what_a_running_runtime_keeps "$build_version"
	say "Tatolab $build_version is installed. Start the runtime in a terminal of its own with:"
	say "  tatolabd"
	say "then a stream with:"
	say "  tatolab new my-stream"
}

uninstall_tatolab() {
	refuse_a_machine_with_no_runtime_build
	executable_links_into_a_build | while read -r link_name _; do
		executable_link="$TATOLAB_EXECUTABLE_LINK_DIRECTORY/$link_name"
		case "$(readlink "$executable_link" 2>/dev/null || true)" in
		"$TATOLAB_INSTALL_DIRECTORY"/*) rm -f "$executable_link" ;;
		esac
	done
	rm -rf "$TATOLAB_INSTALL_DIRECTORY"
	say "Tatolab is uninstalled. Its state directory, with your streams' records and logs, is left in place:"
	say "  $TATOLAB_STATE_DIRECTORY"
}

case "${1:-}" in
'') install_tatolab ;;
--uninstall) uninstall_tatolab ;;
-h | --help) usage_line ;;
*) refuse "unknown argument \`$1\`; $(usage_line)" ;;
esac
