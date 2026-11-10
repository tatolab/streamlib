#!/usr/bin/env bash
#
# Install apt packages under a wall-clock bound, escaping to a second mirror
# rather than waiting out a slow one.
#
# The mode this exists for is a *slow* mirror, not a stalled one. A measured run
# fetched 35.6 MB at 48 kB/s over 12m17s while every request made forward
# progress, so neither of apt's own guards engages: `Acquire::Retries` needs a
# failure to retry and `Acquire::*::Timeout` bounds inactivity, and there was
# neither. A wall-clock bound is the only thing that detects that mode, and a
# different mirror is the only thing that recovers from it — a second try
# against the same host just spends the budget again at the same 48 kB/s.
#
# That is also why there is no attempt-count dial. `Acquire::Retries` already
# covers transient per-file errors inside a single run, so retrying the primary
# would be redundant: two attempts, one per mirror.
#
# Both mirrors were measured slow at once (3.5 MB at 30 kB/s on the fallback,
# with the primary stalled), and no mirror choice recovers from that. So when
# the caller hands over a directory of .debs a previous run downloaded, the
# first attempt installs from those alone with no network, and only a refusal
# there (an image roll moved a dependency, a .deb is missing) reaches a mirror.
# A network install with that directory set downloads into it, so the caller
# can keep what it fetched for the next run.
#
# 120s bounds one apt command and 60s the dpkg repair, and each carries a 10s
# SIGKILL grace on top. The offline install, two mirrors × (update + install)
# and two repairs is therefore 5×130 + 2×70 = 790s worst case — inside the
# caller's
# `timeout-minutes`, which is what lets this script report the failure itself
# instead of being killed mid-sentence. 120s is also ~8× the median step and
# ~1.8× the slowest *successful* update on record, so a merely-mediocre mirror
# still finishes on the primary.
#
# Env (all optional; the last five exist so the gate tests can drive this
# without root, apt, or a network):
#   STREAMLIB_APT_DEB_CACHE_DIRECTORY      .debs to install offline first, and where a network install downloads to
#   STREAMLIB_APT_ATTEMPT_TIMEOUT_SECONDS  bound on one apt command (default 120)
#   STREAMLIB_APT_FALLBACK_MIRROR_URL      mirror used once the primary blows the bound
#   STREAMLIB_APT_PRIVILEGE_PREFIX         how to become root (default `sudo`; may be empty)
#   STREAMLIB_APT_GET_COMMAND              the apt-get to invoke
#   STREAMLIB_APT_MIRROR_SWITCH_COMMAND    the command that repoints apt at the fallback
#   STREAMLIB_DPKG_REPAIR_COMMAND          the command that finishes an interrupted dpkg
#   STREAMLIB_APT_KILL_AFTER_SECONDS       SIGKILL grace after the signal (default 10)

set -euo pipefail

# `timeout` reports 124 when the command honoured the signal and exited, and 137
# (128 + SIGKILL) when `--kill-after` had to escalate. Both mean the bound fired.
readonly TIMEOUT_EXIT_STATUS=124
readonly SIGKILL_ESCALATION_EXIT_STATUS=137
readonly DPKG_REPAIR_TIMEOUT_SECONDS=60

attempt_timeout_seconds="${STREAMLIB_APT_ATTEMPT_TIMEOUT_SECONDS:-120}"
fallback_mirror_url="${STREAMLIB_APT_FALLBACK_MIRROR_URL:-http://archive.ubuntu.com/ubuntu/}"
apt_privilege_prefix="${STREAMLIB_APT_PRIVILEGE_PREFIX-sudo}"
apt_get_command="${STREAMLIB_APT_GET_COMMAND:-apt-get}"
mirror_switch_command="${STREAMLIB_APT_MIRROR_SWITCH_COMMAND:-}"
dpkg_repair_command="${STREAMLIB_DPKG_REPAIR_COMMAND:-dpkg --configure -a}"
kill_after_seconds="${STREAMLIB_APT_KILL_AFTER_SECONDS:-10}"
deb_cache_directory="${STREAMLIB_APT_DEB_CACHE_DIRECTORY:-}"

if [ "$#" -eq 0 ]; then
  echo "usage: ${0##*/} <apt-package>..." >&2
  exit 2
fi

requested_packages=("$@")

# Retries covers the transient per-file failure; the Timeout pair covers a
# connection that goes silent. apt keys Timeout per scheme and the runner's
# sources are a mix — Ubuntu over http, several vendor repos over https — so
# setting only one of them leaves half the fetch unbounded. Neither reaches a
# mirror that is merely slow; that is what the wall-clock bound below is for.
apt_acquire_options=(
  -o Acquire::Retries=3
  -o Acquire::http::Timeout=30
  -o Acquire::https::Timeout=30
)

# apt moves each finished download out of `partial/` into the archives
# directory, and refuses to start when `partial/` is missing.
apt_archive_options=()
if [ -n "$deb_cache_directory" ]; then
  mkdir -p "${deb_cache_directory}/partial"
  apt_archive_options=(-o "Dir::Cache::Archives=${deb_cache_directory}/")
fi

# `sudo timeout ...`, never `timeout sudo ...`: with sudo on the outside the
# signal lands on sudo, which relays SIGINT but cannot be made to pass SIGKILL
# on, so a `--kill-after` would leave an orphaned root apt-get holding
# /var/lib/dpkg/lock-frontend and the fallback attempt would fail on the lock.
# Running timeout as root puts it in the parent slot of apt-get itself.
#
# SIGINT first, because apt unwinds on it and leaves
# /var/cache/apt/archives/partial intact for the next attempt to resume from;
# SIGKILL only if it refuses to go — which is why the escalation status counts
# as a timeout too, in `describe_attempt_failure` below.
run_one_bounded_apt_attempt() {
  local attempt_label="$1"
  local exit_status=0

  echo "==> apt attempt: ${attempt_label} (each command bounded to ${attempt_timeout_seconds}s)"

  # Unquoted on purpose: each may be several words, or empty.
  # shellcheck disable=SC2086
  $apt_privilege_prefix timeout --signal=INT --kill-after="${kill_after_seconds}s" "${attempt_timeout_seconds}s" \
    $apt_get_command update "${apt_acquire_options[@]}" || exit_status=$?

  if [ "$exit_status" -ne 0 ]; then
    return "$exit_status"
  fi

  # shellcheck disable=SC2086
  $apt_privilege_prefix timeout --signal=INT --kill-after="${kill_after_seconds}s" "${attempt_timeout_seconds}s" \
    $apt_get_command install -y "${apt_acquire_options[@]}" ${apt_archive_options[@]+"${apt_archive_options[@]}"} "${requested_packages[@]}" \
    || exit_status=$?

  return "$exit_status"
}

# The .debs go on the command line as paths, which makes each one the candidate
# for its package whatever the image's package lists hold (or whether it holds
# any), and with the cache as the archives directory apt finds each of them
# already downloaded. `--no-download` cannot be the guard: it refuses the copy
# of a local .deb too. A proxy nothing listens on refuses every real fetch
# instead, and apt fetches everything before dpkg runs, so a set that does not
# cover the install fails in about a second having changed nothing. A .deb
# older than what the image holds is refused as well: `-y` permits no downgrade.
#
# apt reads a local .deb as its `_apt` sandbox user, who cannot enter the
# runner's temp directory, so this one install runs unsandboxed.
readonly APT_OPTIONS_REFUSING_EVERY_FETCH=(
  -o Acquire::http::Proxy=http://127.0.0.1:9
  -o Acquire::https::Proxy=http://127.0.0.1:9
  -o Acquire::Retries=0
  -o APT::Sandbox::User=root
)

install_from_the_deb_cache_without_the_network() {
  local cached_debs=("$@")

  echo "==> apt attempt: ${#cached_debs[@]} cached .debs, no network (bounded to ${attempt_timeout_seconds}s)"

  # shellcheck disable=SC2086
  $apt_privilege_prefix timeout --signal=INT --kill-after="${kill_after_seconds}s" "${attempt_timeout_seconds}s" \
    $apt_get_command install -y "${APT_OPTIONS_REFUSING_EVERY_FETCH[@]}" "${apt_archive_options[@]}" \
    "${requested_packages[@]}" "${cached_debs[@]}"
}

# `autoclean` removes each .deb the fresh package lists no longer offer, so the
# directory a caller keeps holds this image's set rather than every set before it.
prune_debs_the_mirror_no_longer_offers() {
  if [ -z "$deb_cache_directory" ]; then
    return
  fi
  # shellcheck disable=SC2086
  $apt_privilege_prefix timeout --signal=INT --kill-after="${kill_after_seconds}s" "${DPKG_REPAIR_TIMEOUT_SECONDS}s" \
    $apt_get_command autoclean ${apt_archive_options[@]+"${apt_archive_options[@]}"} || true
}

# A missing package and a stalled mirror are different diagnoses, and reporting
# the second for the first sends the reader hunting a network problem that is
# not there — the likeliest cause of a deterministic failure here is a
# version-pinned package name that a runner-image roll retired.
describe_attempt_failure() {
  if [ "$1" -eq "$TIMEOUT_EXIT_STATUS" ] || [ "$1" -eq "$SIGKILL_ESCALATION_EXIT_STATUS" ]; then
    echo "did not finish inside the ${attempt_timeout_seconds}s bound"
  else
    echo "failed with apt exit status $1"
  fi
}

switch_apt_to_fallback_mirror() {
  if [ -n "$mirror_switch_command" ]; then
    # shellcheck disable=SC2086
    $mirror_switch_command "$fallback_mirror_url"
    return
  fi

  # GitHub's Ubuntu images point apt at a mirrorlist file rather than at a host
  # (`URIs: mirror+file:/etc/apt/apt-mirrors.txt`), so the whole switch is one
  # file — rewriting sources.list would not move anything.
  if [ -f /etc/apt/apt-mirrors.txt ]; then
    # shellcheck disable=SC2086
    printf '%s\n' "$fallback_mirror_url" \
      | $apt_privilege_prefix tee /etc/apt/apt-mirrors.txt >/dev/null
    echo "==> repointed /etc/apt/apt-mirrors.txt at ${fallback_mirror_url}"
  else
    echo "==> no /etc/apt/apt-mirrors.txt to repoint; the retry re-runs against the same mirror" >&2
  fi
}

# The bound can fire while apt is unpacking rather than downloading, and the
# SIGINT reaches dpkg too. apt then refuses every later install with "dpkg was
# interrupted, you must manually run dpkg --configure -a" — which would make the
# fallback attempt fail deterministically and turn the escape hatch into a no-op.
#
# Bounded like everything else, and for the same reason: this runs right after a
# root apt-get was signalled, so it is exactly when /var/lib/dpkg/lock-frontend
# is most likely to still be held. An unbounded repair here would blow the
# worst case the header states and hand the kill back to `timeout-minutes`.
# 60s because it is local work with no network in it.
finish_any_interrupted_dpkg() {
  # shellcheck disable=SC2086
  $apt_privilege_prefix timeout --signal=INT --kill-after="${kill_after_seconds}s" "${DPKG_REPAIR_TIMEOUT_SECONDS}s" \
    $dpkg_repair_command || true
}

if [ -n "$deb_cache_directory" ]; then
  shopt -s nullglob
  cached_debs=("${deb_cache_directory}"/*.deb)
  shopt -u nullglob

  if [ "${#cached_debs[@]}" -gt 0 ]; then
    offline_status=0
    install_from_the_deb_cache_without_the_network "${cached_debs[@]}" || offline_status=$?
    if [ "$offline_status" -eq 0 ]; then
      exit 0
    fi
    echo "==> the cached .debs $(describe_attempt_failure "$offline_status"); fetching from the primary mirror" >&2
    finish_any_interrupted_dpkg
  fi
fi

primary_status=0
run_one_bounded_apt_attempt "primary mirror" || primary_status=$?
if [ "$primary_status" -eq 0 ]; then
  prune_debs_the_mirror_no_longer_offers
  exit 0
fi

echo "==> primary mirror $(describe_attempt_failure "$primary_status"); escaping to ${fallback_mirror_url}" >&2
finish_any_interrupted_dpkg
switch_apt_to_fallback_mirror

fallback_status=0
run_one_bounded_apt_attempt "fallback mirror" || fallback_status=$?
if [ "$fallback_status" -eq 0 ]; then
  prune_debs_the_mirror_no_longer_offers
  exit 0
fi

echo "==> fallback mirror $(describe_attempt_failure "$fallback_status"); giving up" >&2
exit 1
