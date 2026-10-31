#!/bin/sh
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1
#
# Installs this machine's Tatolab runtime and keeps it running as a per-user
# service: a systemd user service on Linux, a launch agent on macOS.
#
#   curl -fsSL https://get.tatolab.com/install.sh | sh     (Linux)
#   install.sh --from-unit <runtime unit directory>        (Linux and macOS)
#
# The first downloads a release; the second installs a runtime unit it is
# handed, which is how the Tatolab app installs the runtime it carries. A Linux
# unit is the release tarball's bin/, lib/ and `streamlib-commit`, the commit it
# was built from; a macOS unit is `Tatolab Runtime.app`, carrying that file in
# its Contents/Resources/.
#
# Each build is installed into a folder of its own, <version>-<commit>, and
# `current` names the one running; the service names only `current`. An install
# stops the service, points `current` at the build, starts the service, and
# waits until the runtime answers or the service manager reports it exited. No
# deadline ends that wait. An exit puts the build before it back. The build
# before is kept and older ones are removed. Uninstall with
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

TATOLAB_EXECUTABLE_LINK_DIRECTORY="$HOME/.local/bin"
STREAMLIB_COMMIT_FILE_NAME="streamlib-commit"

TATOLAB_SYSTEMD_USER_UNIT_DIRECTORY="$HOME/.config/systemd/user"
TATOLAB_SYSTEMD_USER_UNIT_NAME="tatolabd.service"

TATOLAB_RUNTIME_HELPER_BUNDLE_NAME="Tatolab Runtime.app"
TATOLAB_LAUNCH_AGENT_LABEL="com.tatolab.tatolabd"
TATOLAB_LAUNCH_AGENT_PLIST="$HOME/Library/LaunchAgents/$TATOLAB_LAUNCH_AGENT_LABEL.plist"
TATOLAB_APP_BUNDLE_IDENTIFIER="com.tatolab.tatolab"

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
	say "usage: install.sh [--from-unit <runtime unit directory> | --uninstall]"
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

# --- this platform's install locations --------------------------------------

use_the_install_locations_of_this_platform() {
	installing_from_a_unit="$1"
	operating_system="$(uname -s)"
	machine_architecture="$(uname -m)"
	case "$operating_system" in
	Linux)
		case "$machine_architecture" in
		x86_64 | amd64) ;;
		*) refuse "there is no Tatolab runtime for $machine_architecture Linux yet; x86_64 is the only Linux build" ;;
		esac
		TATOLAB_INSTALL_DIRECTORY="$HOME/.local/share/tatolab"
		TATOLAB_STATE_DIRECTORY="${XDG_STATE_HOME:-$HOME/.local/state}/tatolab"
		runtime_executable_in_a_build="bin/tatolabd"
		command_line_executable_in_a_build="bin/tatolab"
		streamlib_commit_file_in_a_unit="$STREAMLIB_COMMIT_FILE_NAME"
		;;
	Darwin)
		[ "$installing_from_a_unit" = yes ] ||
			refuse "on macOS, Tatolab will be the Tatolab app, which starts the runtime for you; it has" \
				"not shipped yet. Until it does, build the runtime from source:" \
				"https://github.com/tatolab/streamlib#building-the-runtime-from-source"
		[ "$machine_architecture" = arm64 ] || refuse "there is no Tatolab runtime for $machine_architecture macOS; Apple silicon is the only Mac build"
		TATOLAB_INSTALL_DIRECTORY="$HOME/Library/Application Support/com.tatolab.runtime"
		TATOLAB_STATE_DIRECTORY="$HOME/Library/Application Support/Tatolab"
		runtime_executable_in_a_build="$TATOLAB_RUNTIME_HELPER_BUNDLE_NAME/Contents/MacOS/tatolabd"
		command_line_executable_in_a_build="$TATOLAB_RUNTIME_HELPER_BUNDLE_NAME/Contents/MacOS/tatolab-cli"
		streamlib_commit_file_in_a_unit="$TATOLAB_RUNTIME_HELPER_BUNDLE_NAME/Contents/Resources/$STREAMLIB_COMMIT_FILE_NAME"
		;;
	*) refuse "there is no Tatolab runtime for $operating_system" ;;
	esac
}

# What a runtime unit holds at its root, one per line.
top_level_entries_of_a_unit() {
	case "$operating_system" in
	Linux) printf '%s\n' bin lib "$STREAMLIB_COMMIT_FILE_NAME" ;;
	Darwin) printf '%s\n' "$TATOLAB_RUNTIME_HELPER_BUNDLE_NAME" ;;
	esac
}

# What a runtime unit must carry, one per line.
required_members_of_a_unit() {
	case "$operating_system" in
	Linux)
		printf '%s\n' bin/tatolabd bin/tatolab bin/moq-relay bin/uv lib/tatolab/lend \
			lib/tatolab/lend/tatolab/runtime/_vulkan_driver/lvp_icd.x86_64.json \
			lib/tatolab/watch/index.html \
			"lib/systemd/user/$TATOLAB_SYSTEMD_USER_UNIT_NAME" \
			"$streamlib_commit_file_in_a_unit"
		;;
	Darwin)
		helper_contents="$TATOLAB_RUNTIME_HELPER_BUNDLE_NAME/Contents"
		printf '%s\n' "$helper_contents/MacOS/tatolabd" "$helper_contents/MacOS/tatolab-cli" \
			"$helper_contents/MacOS/moq-relay" "$helper_contents/MacOS/uv" \
			"$helper_contents/Resources/lend/tatolab/runtime/_vulkan_driver/MoltenVK_icd.json" \
			"$helper_contents/Resources/watch/index.html" \
			"$streamlib_commit_file_in_a_unit"
		;;
	esac
}

# The executables linked into ~/.local/bin, as `<link name> <path in a build>` lines.
executable_links_into_a_build() {
	case "$operating_system" in
	Linux) printf '%s\n' "tatolab $command_line_executable_in_a_build" "tatolabd $runtime_executable_in_a_build" ;;
	Darwin) printf '%s\n' "tatolab $command_line_executable_in_a_build" ;;
	esac
}

# --- the per-user service ----------------------------------------------------

the_user_systemd_manager_answers() {
	command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1
}

the_service_manager_answers() {
	case "$operating_system" in
	Linux) the_user_systemd_manager_answers ;;
	Darwin) command -v launchctl >/dev/null 2>&1 ;;
	esac
}

launch_agent_domain() {
	printf 'gui/%s' "$(id -u)"
}

how_to_see_the_service() {
	case "$operating_system" in
	Linux) printf '%s' "\`systemctl --user status $TATOLAB_SYSTEMD_USER_UNIT_NAME\`" ;;
	Darwin) printf '%s' "\`launchctl print $(launch_agent_domain)/$TATOLAB_LAUNCH_AGENT_LABEL\`" ;;
	esac
}

how_the_service_runs() {
	case "$operating_system" in
	Linux) printf '%s' "the systemd user service $TATOLAB_SYSTEMD_USER_UNIT_NAME" ;;
	Darwin) printf '%s' "the launch agent $TATOLAB_LAUNCH_AGENT_LABEL" ;;
	esac
}

text_as_xml() {
	printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
}

write_the_launch_agent_plist() {
	mkdir -p "$(dirname "$TATOLAB_LAUNCH_AGENT_PLIST")"
	cat >"$TATOLAB_LAUNCH_AGENT_PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>$TATOLAB_LAUNCH_AGENT_LABEL</string>
	<key>Program</key>
	<string>$(text_as_xml "$TATOLAB_INSTALL_DIRECTORY/current/$runtime_executable_in_a_build")</string>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>SoftResourceLimits</key>
	<dict>
		<key>NumberOfFiles</key>
		<integer>65536</integer>
	</dict>
	<key>AssociatedBundleIdentifiers</key>
	<array>
		<string>$TATOLAB_APP_BUNDLE_IDENTIFIER</string>
	</array>
</dict>
</plist>
EOF
}

write_the_service_definition() {
	case "$operating_system" in
	Linux)
		mkdir -p "$TATOLAB_SYSTEMD_USER_UNIT_DIRECTORY"
		cp "$TATOLAB_INSTALL_DIRECTORY/current/lib/systemd/user/$TATOLAB_SYSTEMD_USER_UNIT_NAME" \
			"$TATOLAB_SYSTEMD_USER_UNIT_DIRECTORY/$TATOLAB_SYSTEMD_USER_UNIT_NAME"
		;;
	Darwin) write_the_launch_agent_plist ;;
	esac
}

# Returns once the service manager has stopped the job.
stop_the_service() {
	case "$operating_system" in
	Linux) systemctl --user stop "$TATOLAB_SYSTEMD_USER_UNIT_NAME" 2>/dev/null || true ;;
	Darwin) launchctl bootout "$(launch_agent_domain)/$TATOLAB_LAUNCH_AGENT_LABEL" 2>/dev/null || true ;;
	esac
}

# Each step chained, because a function called where its status is tested
# runs with `set -e` suspended.
start_the_service() {
	case "$operating_system" in
	Linux)
		# A build that kept exiting leaves the unit over its start limit, which would refuse the next start.
		systemctl --user reset-failed "$TATOLAB_SYSTEMD_USER_UNIT_NAME" 2>/dev/null || true
		systemctl --user daemon-reload &&
			systemctl --user enable --quiet "$TATOLAB_SYSTEMD_USER_UNIT_NAME" &&
			systemctl --user start "$TATOLAB_SYSTEMD_USER_UNIT_NAME"
		;;
	Darwin)
		launchctl enable "$(launch_agent_domain)/$TATOLAB_LAUNCH_AGENT_LABEL" &&
			launchctl bootstrap "$(launch_agent_domain)" "$TATOLAB_LAUNCH_AGENT_PLIST"
		;;
	esac
}

systemd_user_service_property() {
	systemctl --user show "$TATOLAB_SYSTEMD_USER_UNIT_NAME" --property="$1" --value 2>/dev/null || true
}

# How many times systemd has restarted the service after it exited.
systemd_user_service_restarts() {
	service_restarts="$(systemd_user_service_property NRestarts)"
	case "$service_restarts" in
	'' | *[!0-9]*) printf '0' ;;
	*) printf '%s' "$service_restarts" ;;
	esac
}

# Whether the service manager reports that the job started last exited:
# systemd restarting it, or holding it failed or stopped; launchd naming an
# exit code for a job it has just bootstrapped, or no longer holding it.
the_service_has_exited() {
	case "$operating_system" in
	Linux)
		[ "$(systemd_user_service_restarts)" -gt "$1" ] && return 0
		case "$(systemd_user_service_property ActiveState)" in
		failed | inactive) return 0 ;;
		esac
		[ "$(systemd_user_service_property SubState)" = auto-restart ]
		;;
	Darwin)
		launch_agent_description="$(launchctl print "$(launch_agent_domain)/$TATOLAB_LAUNCH_AGENT_LABEL" 2>/dev/null)" ||
			return 0
		last_exit_code="$(printf '%s\n' "$launch_agent_description" |
			sed -n 's/^[[:space:]]*last exit code = //p' | head -n 1)"
		[ -n "$last_exit_code" ] && [ "$last_exit_code" != "(never exited)" ]
		;;
	esac
}

# Whether the runtime answering on this user's local API is `$1`'s version.
the_installed_runtime_answers() {
	"$TATOLAB_INSTALL_DIRECTORY/current/$command_line_executable_in_a_build" --version 2>/dev/null |
		grep -q "^runtime $1 ("
}

# Waits until the runtime answers or the service manager reports its job
# exited, and fails on an exit. Each look is a question to the runtime or the
# service manager; no deadline ends the wait.
wait_until_the_runtime_answers_or_its_service_exits() {
	answering_version="$1"
	restarts_when_started="0"
	[ "$operating_system" = Linux ] && restarts_when_started="$(systemd_user_service_restarts)"
	while :; do
		the_installed_runtime_answers "$answering_version" && return 0
		the_service_has_exited "$restarts_when_started" && return 1
		sleep 0.2
	done
}

# --- the build ---------------------------------------------------------------

# The build a staged runtime unit is, `<version>-<commit>`: its runtime's
# version and the streamlib commit it names.
build_of_the_staged_unit() {
	staged_unit_directory="$1"
	runtime_version_line="$("$staged_unit_directory/$runtime_executable_in_a_build" --version 2>&1)" ||
		refuse "the runtime unit's tatolabd does not run here: $runtime_version_line"
	unit_version="${runtime_version_line#tatolabd }"
	is_a_version "$unit_version" || refuse "the runtime unit's tatolabd names no version: $runtime_version_line"
	unit_commit="$(tr -d ' \t\r\n' <"$staged_unit_directory/$streamlib_commit_file_in_a_unit")"
	is_a_full_commit "$unit_commit" ||
		refuse "the runtime unit's $streamlib_commit_file_in_a_unit names no full commit: \`$unit_commit\`"
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

copy_the_unit_into() {
	say "Copying the runtime unit from $unit_source_directory"
	top_level_entries_of_a_unit | while IFS= read -r unit_entry; do
		[ -e "$unit_source_directory/$unit_entry" ] || continue
		case "$operating_system" in
		Darwin) ditto "$unit_source_directory/$unit_entry" "$1/$unit_entry" ;;
		*) cp -R "$unit_source_directory/$unit_entry" "$1/$unit_entry" ;;
		esac
	done
	unit_description="the runtime unit at $unit_source_directory"
}

# Stages the unit, checks it, and leaves it in its build's folder, named in `build`.
stage_the_unit_into_its_build_folder() {
	mkdir -p "$TATOLAB_INSTALL_DIRECTORY"
	partial_build_directory="$TATOLAB_INSTALL_DIRECTORY/.partial-$$"
	rm -rf "$partial_build_directory"
	mkdir "$partial_build_directory"
	if [ -n "$unit_source_directory" ]; then
		copy_the_unit_into "$partial_build_directory"
	else
		download_the_release_into "$partial_build_directory"
	fi
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
	write_the_service_definition
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

offer_to_keep_the_runtime_running_without_a_login() {
	[ "$operating_system" = Linux ] || return 0
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

put_the_previous_build_back() {
	failed_build="$1"
	previous_build="$2"
	if [ -n "$previous_build" ] && [ "$previous_build" != "$failed_build" ] &&
		[ -d "$TATOLAB_INSTALL_DIRECTORY/$previous_build" ]; then
		say "Tatolab $failed_build did not start; putting $previous_build back"
		stop_the_service
		point_current_at "$previous_build"
		start_the_service || true
		rm -rf "${TATOLAB_INSTALL_DIRECTORY:?}/$failed_build"
		refuse "Tatolab $failed_build's service did not start, so $previous_build is back in place. See" \
			"$(how_to_see_the_service)."
	fi
	refuse "Tatolab $failed_build is installed, but its service did not start. See $(how_to_see_the_service)."
}

install_tatolab() {
	unit_source_directory="$1"
	if [ -n "$unit_source_directory" ]; then
		use_the_install_locations_of_this_platform yes
		[ -d "$unit_source_directory" ] || refuse "\`$unit_source_directory\` is not a runtime unit's directory"
		unit_source_directory="$(cd "$unit_source_directory" && pwd)"
	else
		use_the_install_locations_of_this_platform no
		require_command curl
		require_command tar
	fi
	download_directory="$(mktemp -d)"
	trap 'rm -rf "$download_directory" "$TATOLAB_INSTALL_DIRECTORY/.partial-$$"' EXIT

	stage_the_unit_into_its_build_folder
	build_version="${build%-*}"
	previous_build="$(readlink "$TATOLAB_INSTALL_DIRECTORY/current" 2>/dev/null || true)"
	if [ -n "$previous_build" ] && [ "$previous_build" != "$build" ]; then
		ln -sfn "$previous_build" "$TATOLAB_INSTALL_DIRECTORY/previous"
	fi
	if ! the_service_manager_answers; then
		point_current_at "$build"
		warn "no systemd user manager answers here, so the runtime is installed but not registered" \
			"as a service. Start it with \`tatolabd\` in a terminal of its own."
		say_where_the_executables_are
		return 0
	fi

	say "Stopping the runtime"
	stop_the_service
	point_current_at "$build"
	say "Starting Tatolab $build"
	start_the_service || put_the_previous_build_back "$build" "$previous_build"
	say "Waiting for Tatolab $build to answer"
	wait_until_the_runtime_answers_or_its_service_exits "$build_version" ||
		put_the_previous_build_back "$build" "$previous_build"
	say "Tatolab $build answers, running as $(how_the_service_runs)"
	remove_every_build_but "$build" "$(readlink "$TATOLAB_INSTALL_DIRECTORY/previous" 2>/dev/null || true)"
	offer_to_keep_the_runtime_running_without_a_login
	say_where_the_executables_are
	say "Tatolab $build_version is installed. Start a stream with:"
	say "  tatolab new my-stream"
}

uninstall_tatolab() {
	use_the_install_locations_of_this_platform yes
	case "$operating_system" in
	Linux)
		if the_user_systemd_manager_answers; then
			systemctl --user disable --now --quiet "$TATOLAB_SYSTEMD_USER_UNIT_NAME" 2>/dev/null || true
		fi
		rm -f "$TATOLAB_SYSTEMD_USER_UNIT_DIRECTORY/$TATOLAB_SYSTEMD_USER_UNIT_NAME"
		if the_user_systemd_manager_answers; then
			systemctl --user daemon-reload
		fi
		;;
	Darwin)
		stop_the_service
		rm -f "$TATOLAB_LAUNCH_AGENT_PLIST"
		;;
	esac
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
'') install_tatolab "" ;;
--from-unit)
	[ $# -ge 2 ] && [ -n "$2" ] || refuse "--from-unit needs the runtime unit's directory; $(usage_line)"
	install_tatolab "$2"
	;;
--uninstall) uninstall_tatolab ;;
-h | --help) usage_line ;;
*) refuse "unknown argument \`$1\`; $(usage_line)" ;;
esac
