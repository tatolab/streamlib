#!/bin/sh
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Installs this machine's Tatolab runtime and keeps it running as a systemd
# user service:
#
#   curl -fsSL https://get.tatolab.com/install.sh | sh
#
# Re-running it upgrades in place and restarts the service. Uninstall with
#
#   curl -fsSL https://get.tatolab.com/install.sh | sh -s -- --uninstall
#
# which removes the service and the installed files and never the state
# directory, where the runtime keeps its streams and logs.
#
# TATOLAB_INSTALL_VERSION installs that release rather than the newest one.
# TATOLAB_INSTALL_BASE_URL serves the releases from somewhere else, which is
# how the installer's own tests run it against a release they made.

set -eu

TATOLAB_INSTALL_BASE_URL="${TATOLAB_INSTALL_BASE_URL:-https://get.tatolab.com}"

TATOLAB_INSTALL_DIRECTORY="$HOME/.local/share/tatolab"
TATOLAB_EXECUTABLE_LINK_DIRECTORY="$HOME/.local/bin"
TATOLAB_SYSTEMD_USER_UNIT_DIRECTORY="$HOME/.config/systemd/user"
TATOLAB_SYSTEMD_USER_UNIT_NAME="tatolabd.service"
TATOLAB_RUNTIME_EXECUTABLE_NAMES="tatolab tatolabd"

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

the_user_systemd_manager_answers() {
	command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1
}

refuse_a_platform_without_a_linux_build() {
	operating_system="$(uname -s)"
	case "$operating_system" in
	Linux) ;;
	Darwin)
		refuse "on macOS, Tatolab is the Tatolab app, which starts the runtime for you:" \
			"\`brew install --cask tatolab/tap/tatolab\`, or the .dmg on" \
			"https://github.com/tatolab/streamlib/releases/latest"
		;;
	*) refuse "there is no Tatolab runtime for $operating_system" ;;
	esac
	machine_architecture="$(uname -m)"
	case "$machine_architecture" in
	x86_64 | amd64) ;;
	*) refuse "there is no Tatolab runtime for $machine_architecture Linux yet; x86_64 is the only Linux build" ;;
	esac
}

release_version_to_install() {
	if [ -n "${TATOLAB_INSTALL_VERSION:-}" ]; then
		release_version="${TATOLAB_INSTALL_VERSION#v}"
	else
		download "$TATOLAB_INSTALL_BASE_URL/latest" "$download_directory/latest"
		release_version="$(tr -d ' \t\r\n' <"$download_directory/latest")"
	fi
	case "$release_version" in
	'' | *[!0-9A-Za-z.+-]* | .* | -*)
		refuse "\`$release_version\` is not a release version"
		;;
	esac
	printf '%s' "$release_version"
}

unpack_the_release() {
	release_version="$1"
	release_directory="$TATOLAB_INSTALL_DIRECTORY/$release_version"
	if [ -x "$release_directory/bin/tatolabd" ] && [ -x "$release_directory/bin/tatolab" ]; then
		say "Tatolab $release_version is already unpacked in $release_directory"
		return
	fi

	tarball_name="tatolab-runtime-$release_version-x86_64-linux.tar.gz"
	tarball_url="$TATOLAB_INSTALL_BASE_URL/releases/$release_version/$tarball_name"
	say "Downloading Tatolab $release_version"
	download "$tarball_url" "$download_directory/$tarball_name"
	download "$tarball_url.sha256" "$download_directory/$tarball_name.sha256"
	expected_sha256="$(cut -d ' ' -f 1 <"$download_directory/$tarball_name.sha256")"
	actual_sha256="$(sha256_of_file "$download_directory/$tarball_name")"
	[ "$actual_sha256" = "$expected_sha256" ] ||
		refuse "$tarball_name does not match its published sha256 (got $actual_sha256, expected $expected_sha256)"

	mkdir -p "$TATOLAB_INSTALL_DIRECTORY"
	partial_release_directory="$TATOLAB_INSTALL_DIRECTORY/.$release_version.partial"
	rm -rf "$partial_release_directory"
	mkdir "$partial_release_directory"
	tar -xzf "$download_directory/$tarball_name" -C "$partial_release_directory"
	for unpacked_member in bin/tatolabd bin/tatolab lib/tatolab/lend "lib/systemd/user/$TATOLAB_SYSTEMD_USER_UNIT_NAME"; do
		[ -e "$partial_release_directory/$unpacked_member" ] || {
			rm -rf "$partial_release_directory"
			refuse "$tarball_name carries no $unpacked_member"
		}
	done
	rm -rf "$release_directory"
	mv "$partial_release_directory" "$release_directory"
}

point_current_at() {
	ln -sfn "$1" "$TATOLAB_INSTALL_DIRECTORY/current"
	mkdir -p "$TATOLAB_EXECUTABLE_LINK_DIRECTORY"
	for executable_name in $TATOLAB_RUNTIME_EXECUTABLE_NAMES; do
		ln -sfn "$TATOLAB_INSTALL_DIRECTORY/current/bin/$executable_name" \
			"$TATOLAB_EXECUTABLE_LINK_DIRECTORY/$executable_name"
	done
	mkdir -p "$TATOLAB_SYSTEMD_USER_UNIT_DIRECTORY"
	cp "$TATOLAB_INSTALL_DIRECTORY/current/lib/systemd/user/$TATOLAB_SYSTEMD_USER_UNIT_NAME" \
		"$TATOLAB_SYSTEMD_USER_UNIT_DIRECTORY/$TATOLAB_SYSTEMD_USER_UNIT_NAME"
}

start_the_service() {
	if ! the_user_systemd_manager_answers; then
		warn "no systemd user manager answers here, so the runtime is installed but not registered" \
			"as a service. Start it with \`tatolabd\` in a terminal of its own."
		return 1
	fi
	systemctl --user daemon-reload
	systemctl --user enable --quiet "$TATOLAB_SYSTEMD_USER_UNIT_NAME"
	systemctl --user restart "$TATOLAB_SYSTEMD_USER_UNIT_NAME"
	say "tatolabd is running as the systemd user service $TATOLAB_SYSTEMD_USER_UNIT_NAME"
}

remove_every_release_but() {
	for installed_release_directory in "$TATOLAB_INSTALL_DIRECTORY"/*; do
		[ -d "$installed_release_directory" ] || continue
		[ -L "$installed_release_directory" ] && continue
		[ "$(basename "$installed_release_directory")" = "$1" ] && continue
		rm -rf "$installed_release_directory"
	done
}

offer_to_keep_the_runtime_running_without_a_login() {
	command -v loginctl >/dev/null 2>&1 || return 0
	user_name="$(id -un)"
	[ "$(loginctl show-user "$user_name" --property=Linger --value 2>/dev/null)" = "no" ] || return 0
	linger_command="loginctl enable-linger $user_name"
	if (exec </dev/tty) 2>/dev/null; then
		printf 'Keep the runtime running while you are logged out, and start it at boot? (%s) [y/N] ' \
			"$linger_command" >/dev/tty
		read -r linger_answer </dev/tty || linger_answer=""
		case "$linger_answer" in
		y | Y | yes | YES)
			$linger_command || warn "\`$linger_command\` failed; run it yourself to keep the runtime up without a login"
			return 0
			;;
		esac
	fi
	say "The runtime runs while you are logged in. To keep it running on a machine nobody logs in to," \
		"run \`$linger_command\`."
}

say_where_the_executables_are() {
	case ":$PATH:" in
	*":$TATOLAB_EXECUTABLE_LINK_DIRECTORY:"*) ;;
	*) warn "$TATOLAB_EXECUTABLE_LINK_DIRECTORY is not on your PATH; add it to run \`tatolab\`" ;;
	esac
}

install_tatolab() {
	refuse_a_platform_without_a_linux_build
	require_command curl
	require_command tar
	download_directory="$(mktemp -d)"
	trap 'rm -rf "$download_directory"' EXIT

	release_version="$(release_version_to_install)"
	unpack_the_release "$release_version"
	point_current_at "$release_version"
	if start_the_service; then
		remove_every_release_but "$release_version"
		offer_to_keep_the_runtime_running_without_a_login
	fi
	say_where_the_executables_are
	say "Tatolab $release_version is installed. Start a stream with:"
	say "  tatolab new my-stream && cd my-stream && uv sync && tatolab run -d"
}

uninstall_tatolab() {
	if the_user_systemd_manager_answers; then
		systemctl --user disable --now --quiet "$TATOLAB_SYSTEMD_USER_UNIT_NAME" 2>/dev/null || true
	fi
	rm -f "$TATOLAB_SYSTEMD_USER_UNIT_DIRECTORY/$TATOLAB_SYSTEMD_USER_UNIT_NAME"
	if the_user_systemd_manager_answers; then
		systemctl --user daemon-reload
	fi
	for executable_name in $TATOLAB_RUNTIME_EXECUTABLE_NAMES; do
		executable_link="$TATOLAB_EXECUTABLE_LINK_DIRECTORY/$executable_name"
		case "$(readlink "$executable_link" 2>/dev/null || true)" in
		"$TATOLAB_INSTALL_DIRECTORY"/*) rm -f "$executable_link" ;;
		esac
	done
	rm -rf "$TATOLAB_INSTALL_DIRECTORY"
	say "Tatolab is uninstalled. Its state directory, with your kept streams and logs, is left in place:"
	say "  ${XDG_STATE_HOME:-$HOME/.local/state}/tatolab"
}

case "${1:-}" in
'') install_tatolab ;;
--uninstall) uninstall_tatolab ;;
-h | --help)
	say "usage: install.sh [--uninstall]"
	;;
*) refuse "unknown argument \`$1\`; usage: install.sh [--uninstall]" ;;
esac
