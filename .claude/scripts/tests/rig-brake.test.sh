#!/usr/bin/env bash
# Unit tests for .claude/hooks/rig-brake.sh.
#
# The hook decides one thing: does this Bash command look like it drives the rig,
# and should therefore reach the human? Two failures matter and they pull in
# opposite directions. A key that can never fire is a gate that reports safe on a
# command nobody vetted. A key that fires on a benign probe trains the owner to
# click through, which costs every other key its meaning. So both directions get
# cases here: what must escalate, and what must stay silent.
#
# No toolchain, no network: bash + jq.
set -uo pipefail

hook="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/hooks/rig-brake.sh"
[ -f "$hook" ] || { echo "hook not found at $hook" >&2; exit 2; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 2; }

passed=0
failed=0
out=""
status=0

# run_hook <command> [cwd] — feed the hook one synthetic PreToolUse tool input,
# exactly the JSON shape Claude Code hands it on a Bash call.
run_hook() {
  out="$(jq -n --arg c "$1" --arg d "${2:-/home/dev/streamlib}" \
    '{tool_input: {command: $c}, cwd: $d}' | bash "$hook" 2>&1)"
  status=$?
}

ok() { passed=$((passed + 1)); printf '  ok   %s\n' "$1"; }
bad() {
  failed=$((failed + 1))
  printf '  FAIL %s\n' "$1"
  printf '%s\n' "$out" | sed 's/^/       | /'
}

# The escalation contract: exit 0 carrying permissionDecision "ask". Never exit 2
# — a hard deny is the final say this hook deliberately does not take.
expect_ask() {
  local decision
  decision="$(printf '%s' "$out" | jq -r '.hookSpecificOutput.permissionDecision // ""' 2>/dev/null)"
  if [ "$status" -eq 0 ] && [ "$decision" = "ask" ]; then
    ok "$1"
  else
    bad "$1 (expected exit 0 + ask, got exit $status decision '${decision:-none}')"
  fi
}

# Silence is deference to the normal permission flow, and it must be total:
# any stdout at all is a decision.
expect_silent() {
  if [ "$status" -eq 0 ] && [ -z "$out" ]; then
    ok "$1"
  else
    bad "$1 (expected exit 0 + no output, got exit $status)"
  fi
}

echo "rig-brake.sh"

# ── The Python launch path ───────────────────────────────────────────
# `streamlib run` / `dev` boots a real node: camera, GPU, and a window. It is
# the launch path that replaced `cargo run -p <example>`, and the examples the
# rig scenarios use are Python apps now.
run_hook 'streamlib run --dir examples/camera-display'
expect_ask "streamlib run of an app under examples/ escalates"

run_hook 'streamlib dev --dir examples/camera-python-effects'
expect_ask "streamlib dev of an app under examples/ escalates"

run_hook 'STREAMLIB_CAMERA_DEVICE=/dev/video0 streamlib run --dir examples/camera-display'
expect_ask "an env-prefixed streamlib run still escalates"

# The wheel venv's CLI, named by path, is what a session working in-tree has —
# there is no `streamlib` on PATH until a wheel is installed.
run_hook '/home/dev/streamlib/sdk/streamlib-python-wheel/.venv/bin/streamlib run --dir /home/dev/streamlib/examples/camera-display'
expect_ask "the wheel venv CLI named by absolute path still escalates"

run_hook 'streamlib run' '/home/dev/streamlib/examples/camera-display'
expect_ask "streamlib run from inside an example directory escalates"

# ── The observation verbs stay silent ────────────────────────────────
# This is load-bearing, not politeness: /verify-live's own procedure taps a
# channel and exchanges surface ids. If those prompted, the audit flow would
# stall on every frame it reads.
run_hook 'streamlib exchange --channel CyberpunkGlitch/video_to_downstream --out /tmp/e2e --count 3'
expect_silent "streamlib exchange is a control-plane read, not a rig command"

run_hook 'streamlib tap camera/video --count 5'
expect_silent "streamlib tap is a control-plane read"

run_hook 'streamlib nodes'
expect_silent "streamlib nodes is a registry read"

run_hook 'streamlib graph --node abc123'
expect_silent "streamlib graph is a control-plane read"

run_hook 'streamlib logs abc123'
expect_silent "streamlib logs is a control-plane read"

# Prose mentioning the launch path is not the launch path. A session greps its
# own skill text constantly; quoting `streamlib run` must not cost a prompt.
run_hook 'grep -rn "streamlib run" examples/ .claude/'
expect_silent "grepping for the words streamlib run does not escalate"

# ── cargo run ────────────────────────────────────────────────────────
# examples/* are not workspace members, so no `-p` spelling resolves one. The
# example crates that remain are reached by cwd or manifest path.
run_hook 'cargo run -p camera-display'
expect_silent "cargo run -p camera-display names a crate that no longer exists"

run_hook 'cargo run --release' '/home/dev/streamlib/examples/vulkan-video-roundtrip'
expect_ask "cargo run from inside an example directory escalates"

run_hook 'cargo run --manifest-path examples/vulkan-video-roundtrip/Cargo.toml'
expect_ask "cargo run against an example manifest escalates"

run_hook 'cargo run -p xtask -- check-no-in-process-placement'
expect_silent "cargo run of a workspace tool does not escalate"

# ── Unchanged keys, kept honest ──────────────────────────────────────
run_hook 'ffmpeg -f v4l2 -i /dev/video10 -t 5 out.mp4'
expect_ask "ffmpeg reading a camera device escalates"

run_hook 'runtime/streamlib-engine/tests/fixtures/e2e_camera_display.sh /tmp/streamlib-e2e'
expect_ask "an e2e fixture script escalates"

run_hook 'v4l2-ctl -d /dev/video0 --stream-mmap --stream-count=10'
expect_ask "a v4l2-ctl streaming verb escalates"

run_hook 'v4l2-ctl --list-devices'
expect_silent "a v4l2-ctl query verb falls through"

run_hook 'v4l2-ctl -d /dev/video0 --get-fmt-video'
expect_silent "probing a device format falls through"

# The old false positive this rule was narrowed to kill: a benign read whose
# text merely mentions a device path.
run_hook 'cat /tmp/notes-about-/dev/video0.txt'
expect_silent "a benign command that merely mentions a device path falls through"

run_hook 'cargo build -p streamlib-engine'
expect_silent "an ordinary build falls through"

echo ""
printf '  %d passed, %d failed\n' "$passed" "$failed"
[ "$failed" -eq 0 ]
