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
# 120s bounds one apt command, so two mirrors × (update + install) is a 480s
# worst case — inside the caller's `timeout-minutes`, which is what lets this
# script report the failure itself instead of being killed mid-sentence. 120s is
# also ~8× the median step and ~1.8× the slowest *successful* update on record,
# so a merely-mediocre mirror still finishes on the primary.
#
# Env (all optional; the last four exist so the gate tests can drive this
# without root, apt, or a network):
#   STREAMLIB_APT_ATTEMPT_TIMEOUT_SECONDS  bound on one apt command (default 120)
#   STREAMLIB_APT_FALLBACK_MIRROR_URL      mirror used once the primary blows the bound
#   STREAMLIB_APT_PRIVILEGE_PREFIX         how to become root (default `sudo`; may be empty)
#   STREAMLIB_APT_GET_COMMAND              the apt-get to invoke
#   STREAMLIB_APT_MIRROR_SWITCH_COMMAND    the command that repoints apt at the fallback
#   STREAMLIB_DPKG_REPAIR_COMMAND          the command that finishes an interrupted dpkg

set -euo pipefail

readonly TIMEOUT_EXIT_STATUS=124

attempt_timeout_seconds="${STREAMLIB_APT_ATTEMPT_TIMEOUT_SECONDS:-120}"
fallback_mirror_url="${STREAMLIB_APT_FALLBACK_MIRROR_URL:-http://archive.ubuntu.com/ubuntu/}"
apt_privilege_prefix="${STREAMLIB_APT_PRIVILEGE_PREFIX-sudo}"
apt_get_command="${STREAMLIB_APT_GET_COMMAND:-apt-get}"
mirror_switch_command="${STREAMLIB_APT_MIRROR_SWITCH_COMMAND:-}"
dpkg_repair_command="${STREAMLIB_DPKG_REPAIR_COMMAND:-dpkg --configure -a}"

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

# `sudo timeout ...`, never `timeout sudo ...`: with sudo on the outside the
# signal lands on sudo, which relays SIGINT but cannot be made to pass SIGKILL
# on, so a `--kill-after` would leave an orphaned root apt-get holding
# /var/lib/dpkg/lock-frontend and the fallback attempt would fail on the lock.
# Running timeout as root puts it in the parent slot of apt-get itself.
#
# SIGINT first, because apt unwinds on it and leaves
# /var/cache/apt/archives/partial intact for the next attempt to resume from;
# SIGKILL only if it refuses to go. `timeout` reports 124 when it fires, which
# is what tells a slow mirror apart from a broken package name below.
run_one_bounded_apt_attempt() {
  local attempt_label="$1"
  local exit_status=0

  echo "==> apt attempt: ${attempt_label} (each command bounded to ${attempt_timeout_seconds}s)"

  # Unquoted on purpose: each may be several words, or empty.
  # shellcheck disable=SC2086
  $apt_privilege_prefix timeout --signal=INT --kill-after=10s "${attempt_timeout_seconds}s" \
    $apt_get_command update "${apt_acquire_options[@]}" || exit_status=$?

  if [ "$exit_status" -ne 0 ]; then
    return "$exit_status"
  fi

  # shellcheck disable=SC2086
  $apt_privilege_prefix timeout --signal=INT --kill-after=10s "${attempt_timeout_seconds}s" \
    $apt_get_command install -y "${apt_acquire_options[@]}" "${requested_packages[@]}" \
    || exit_status=$?

  return "$exit_status"
}

# A missing package and a stalled mirror are different diagnoses, and reporting
# the second for the first sends the reader hunting a network problem that is
# not there — the likeliest cause of a deterministic failure here is a
# version-pinned package name that a runner-image roll retired.
describe_attempt_failure() {
  if [ "$1" -eq "$TIMEOUT_EXIT_STATUS" ]; then
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
finish_any_interrupted_dpkg() {
  # shellcheck disable=SC2086
  $apt_privilege_prefix $dpkg_repair_command || true
}

primary_status=0
run_one_bounded_apt_attempt "primary mirror" || primary_status=$?
if [ "$primary_status" -eq 0 ]; then
  exit 0
fi

echo "==> primary mirror $(describe_attempt_failure "$primary_status"); escaping to ${fallback_mirror_url}" >&2
finish_any_interrupted_dpkg
switch_apt_to_fallback_mirror

fallback_status=0
run_one_bounded_apt_attempt "fallback mirror" || fallback_status=$?
if [ "$fallback_status" -eq 0 ]; then
  exit 0
fi

echo "==> fallback mirror $(describe_attempt_failure "$fallback_status"); giving up" >&2
exit 1
