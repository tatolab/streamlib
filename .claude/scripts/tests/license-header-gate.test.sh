#!/usr/bin/env bash
# Unit tests for scripts/check-license-headers.sh.
#
# The gate holds BUSL over every first-party source file while leaving a
# vendored tree's own licence headers alone, and those two pull against each
# other. The exemption is the side that fails silently: an over-wide pathspec
# stops covering first-party code and nothing goes red. So every case here pins
# a path — a vendored tree passes carrying someone else's SPDX id, and a sibling
# one path segment away still fails carrying exactly the same header.
#
# No toolchain, no network: bash + git. Each case is a throwaway repo, because
# the gate discovers files with `git ls-files` and would otherwise read the
# checkout it is running from.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
gate="$repo_root/scripts/check-license-headers.sh"
[ -f "$gate" ] || { echo "gate not found at $gate" >&2; exit 2; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

passed=0
failed=0
out=""
status=0
case_no=0
repo=""

new_repo() {
  case_no=$((case_no + 1))
  repo="$work/case-$case_no"
  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" config user.email t@example.com
  git -C "$repo" config user.name t
}

# plant <path> — contents on stdin. Left untracked: the gate reads
# `--others --exclude-standard` too, so a file you just created is in scope.
plant() {
  mkdir -p "$repo/$(dirname "$1")"
  cat >"$repo/$1"
}

busl_rust() { printf '// Copyright (c) 2025 Jonathan Fontanez\n// SPDX-License-Identifier: BUSL-1.1\n\npub fn f() {}\n'; }
busl_python() { printf '# Copyright (c) 2025 Jonathan Fontanez\n# SPDX-License-Identifier: BUSL-1.1\n\ndef f(): ...\n'; }

# The header moq-transport 0.16.2 actually ships, verbatim.
cloudflare_rust() {
  printf '// SPDX-FileCopyrightText: 2024-2026 Cloudflare Inc., Luke Curley, Mike English and contributors\n'
  printf '// SPDX-License-Identifier: MIT OR Apache-2.0\n\npub fn f() {}\n'
}
apache_rust() { printf '// SPDX-License-Identifier: Apache-2.0\n\npub fn f() {}\n'; }

run_gate() {
  out="$(cd "$repo" && bash "$gate" 2>&1)"
  status=$?
}

ok() { passed=$((passed + 1)); printf '  ok   %s\n' "$1"; }
bad() {
  failed=$((failed + 1))
  printf '  FAIL %s\n' "$1"
  printf '%s\n' "$out" | sed 's/^/       | /'
}

expect_pass() {
  run_gate
  if [ "$status" -eq 0 ]; then ok "$1"; else bad "$1 (expected exit 0, got $status)"; fi
}

# expect_fail_naming <label> <path> — the gate must go red AND name the file,
# because a red run that names the wrong path sends the next session to fix a
# file that was never the problem.
expect_fail_naming() {
  run_gate
  if [ "$status" -eq 1 ] && printf '%s' "$out" | grep -qF "$2"; then
    ok "$1"
  else
    bad "$1 (expected exit 1 naming $2, got $status)"
  fi
}

echo "scripts/check-license-headers.sh"

new_repo
busl_rust | plant runtime/streamlib-engine/src/lib.rs
busl_python | plant sdk/streamlib-python-wheel/python/streamlib/__init__.py
expect_pass "first-party Rust and Python carrying the BUSL header pass"

new_repo
printf 'pub fn f() {}\n' | plant runtime/streamlib-engine/src/lib.rs
expect_fail_naming "a Rust file with no header fails" "runtime/streamlib-engine/src/lib.rs"

new_repo
printf '// Copyright (c) 2025 Jonathan Fontanez\n\npub fn f() {}\n' | plant runtime/streamlib-engine/src/lib.rs
expect_fail_naming "the copyright line without the SPDX line fails" "runtime/streamlib-engine/src/lib.rs"

new_repo
{ printf '#!/usr/bin/env python3\n'; busl_python; } | plant tools/thing.py
expect_pass "a shebang moves the Python header to lines 2-3"

new_repo
apache_rust | plant vendor/tatolab-vulkanalia/src/lib.rs
apache_rust | plant vendor/tatolab-vulkanalia-sys/src/lib.rs
apache_rust | plant vendor/tatolab-vulkanalia-vma/src/lib.rs
expect_pass "the three vendored vulkanalia trees keep their Apache-2.0 headers"

new_repo
apache_rust | plant vendor/tatolab-vulkanalia-extras/src/lib.rs
expect_fail_naming "a vendor/ sibling the exclusions do not name still fails" \
  "vendor/tatolab-vulkanalia-extras/src/lib.rs"

new_repo
cloudflare_rust | plant packages/streamlib-moq/vendor/moq-transport/src/lib.rs
cloudflare_rust | plant packages/streamlib-moq/vendor/moq-transport/src/coding/varint.rs
busl_rust | plant packages/streamlib-moq/src/moq_session.rs
expect_pass "the MoQ wheel's vendored moq-transport keeps its MIT OR Apache-2.0 headers"

new_repo
cloudflare_rust | plant packages/streamlib-moq/src/moq_session.rs
expect_fail_naming "the exemption is the vendored tree, not the wheel around it" \
  "packages/streamlib-moq/src/moq_session.rs"

new_repo
cloudflare_rust | plant packages/streamlib-moq/vendor/some-other-crate/src/lib.rs
expect_fail_naming "a second crate under the same vendor/ dir is not exempt by association" \
  "packages/streamlib-moq/vendor/some-other-crate/src/lib.rs"

# Every exemption is Rust-only, and deliberately so: neither vendored tree ships
# a single `.py`, so the Python check buys its simplicity for free.
new_repo
apache_rust | plant vendor/tatolab-vulkanalia/src/lib.rs
printf 'def f(): ...\n' | plant vendor/tatolab-vulkanalia/generator/gen.py
expect_fail_naming "a Python file inside a vendored tree is not exempt" \
  "vendor/tatolab-vulkanalia/generator/gen.py"

echo
echo "$passed passed, $failed failed"
[ "$failed" -eq 0 ]
