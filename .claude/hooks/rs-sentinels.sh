#!/usr/bin/env bash
# rs-sentinels (PostToolUse / Write|Edit): never blocks (exit 0 always). Nudges when
# a written/edited .rs file is missing the BUSL header or uses println!/eprintln!.
# CI is the real gate — this is just a reminder surfaced into context.

input="$(cat)"
file="$(printf '%s' "$input" | jq -r '.tool_input.file_path // ""')"

case "$file" in
  *.rs) ;;
  *) exit 0 ;;
esac

# Vendored vulkanalia is Apache-2.0 — no BUSL header there.
case "$file" in
  *vendor/tatolab-vulkanalia*) exit 0 ;;
esac

[ -f "$file" ] || exit 0

notes=""
if ! grep -q 'SPDX-License-Identifier: BUSL-1.1' "$file"; then
  notes="missing BUSL header (// Copyright (c) 2025 Jonathan Fontanez + // SPDX-License-Identifier: BUSL-1.1)"
fi
if grep -Eq '(^|[^[:alnum:]_])(println|eprintln)!' "$file"; then
  [ -n "$notes" ] && notes="$notes; "
  notes="${notes}uses println!/eprintln! — logging goes through tracing"
fi

if [ -n "$notes" ]; then
  msg="rs-sentinels: $file — $notes. (CI is the real gate.)"
  jq -cn --arg m "$msg" '{hookSpecificOutput:{hookEventName:"PostToolUse",additionalContext:$m}}'
fi

exit 0
