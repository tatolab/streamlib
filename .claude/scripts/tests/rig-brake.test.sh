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

# Prose mentioning the launch path is not the launch path. A session working on
# this hook writes these constantly; prompting on them is what teaches an owner
# to click through without reading, which costs every other key its meaning.
run_hook 'grep -rn "streamlib run" examples/ .claude/'
expect_silent "grepping for the words streamlib run does not escalate"

run_hook 'git commit -m "fix(cli): streamlib run --dir examples/foo now resolves app.py"'
expect_silent "a commit message quoting the launch path does not escalate"

run_hook 'gh pr create --title docs --body "run it with streamlib run --dir examples/x"'
expect_silent "a PR body quoting the launch path does not escalate"

run_hook 'sed -i "s|streamlib run |streamlib dev |" examples/camera-display/README.md'
expect_silent "rewriting the launch path in a doc does not escalate"

# The accepted cost of that suppression: a compound whose FIRST word is a text
# tool hides a real launch behind it. Rare, and the failure is a benign exit
# 144, where the false positives above are frequent and corrosive. A compound
# led by anything else still escalates — see the `cd` case below.
run_hook 'git status && streamlib run --dir examples/camera-display'
expect_silent "a launch hidden behind a leading text tool is NOT braked (accepted trade-off)"

run_hook 'cd examples/camera-display && streamlib run'
expect_ask "a compound not led by a text tool still escalates"

# ONLY the first command word may suppress. `has` is line-oriented, so testing
# the raw input let any line of a multi-line command silence the launch — and
# the shape /verify-live prescribes (background the node, echo its pid) is
# exactly that. These three were silent until the guard read the first line only.
run_hook 'streamlib run --dir examples/camera-display > /tmp/rig.log 2>&1 &
echo "pid $!"'
expect_ask "a backgrounded launch followed by echo still escalates"

run_hook 'cd examples/camera-display
streamlib run > /tmp/rig.log 2>&1 &
echo started'
expect_ask "a multi-line cd + launch + echo still escalates"

run_hook 'STREAMLIB_CAMERA_DEVICE=/dev/video0 streamlib run --dir examples/camera-display > /tmp/rig.log 2>&1 &
echo "launched $!"'
expect_ask "the skill's own prescribed launch shape escalates"

# A text-tool line ABOVE a launch must not hide it either — the skill tells you
# to probe the rig before self-running, so probe-then-launch is the common
# shape. Only the per-line filter keeps these visible; a first-line-only guard
# reads the preamble and silences the launch below it.
run_hook 'echo starting
streamlib run --dir examples/camera-display &'
expect_ask "an echo preamble does not hide the launch beneath it"

run_hook 'grep -q vivid /proc/modules
streamlib run --dir examples/camera-display &'
expect_ask "probing the rig then launching still escalates"

# Exec wrappers. Bounding an unattended run is precisely what a sandboxed
# firing does, so a launch behind `timeout` or `nohup` is the case this brake
# most needs to catch.
run_hook 'nohup streamlib run --dir examples/camera-display &'
expect_ask "a launch behind nohup escalates"

run_hook 'timeout 30 streamlib run --dir examples/camera-display'
expect_ask "a launch behind timeout escalates"

run_hook 'timeout --kill-after=5 30 streamlib run --dir examples/camera-display'
expect_ask "a launch behind timeout with flags escalates"

run_hook 'uv run streamlib run --dir examples/camera-display'
expect_ask "a launch behind uv run escalates"

run_hook 'DISPLAY=:1 STREAMLIB_CAMERA_DEVICE=/dev/video0 nohup streamlib run --dir examples/camera-display >/tmp/log 2>&1 &'
expect_ask "env assignments plus a wrapper plus redirection still escalates"

run_hook "python3 -c \"print('streamlib run --dir examples/x')\""
expect_silent "printing the launch path from a script is not launching it"

# These two hold the command-position anchoring specifically. Neither command's
# first word is a text tool, so the text-tool filter does not reach them —
# replace the anchored regex with a bare `streamlib[[:space:]]+(run|dev)` and
# these are what go red.
run_hook 'curl -sX POST -d "streamlib run --dir examples/x" http://localhost:9000/notes'
expect_silent "posting the launch path as data is not launching it"

run_hook 'ls examples/ && grep -rn "cargo run" examples/'
expect_silent "listing then grepping for cargo run does not escalate"

# The cargo key is anchored the same way its streamlib twin is; a read that
# merely prints the phrase is not a build.
run_hook 'tail -20 examples/jpeg-psnr/README.md'
expect_silent "reading an example README does not escalate"

# ── Known-uncovered shapes, locked so a change to them fails loudly ──
# A `bash -c "…"` body is an unparsed string: any rule that reaches a launch
# inside it also reaches inside `bash -c "grep -rn 'streamlib run' examples/"`.
run_hook 'bash -c "streamlib run --dir examples/camera-display"'
expect_silent "a launch inside bash -c is NOT braked (unparsed string body)"

# The rig's own runnable is an engine example — a workspace target reached by
# `--example`, not a crate under examples/ — so the path key never sees it,
# and it opens a camera, a Vulkan Video queue and a display window unbraked.
# The gap arrived with the rig, not with a change to this hook. Locked so
# closing it fails loudly here rather than silently widening the key.
run_hook 'cargo run -p streamlib-engine --example codec_roundtrip_rig -- --source camera'
expect_silent "the codec rig is NOT braked (it names no examples/ path)"

# A heredoc body line is indistinguishable from a command line, so writing an
# evidence report that quotes the launch command asks. This is the one residual
# FALSE POSITIVE, and it fires on the skill's own E2E report template.
run_hook 'cat > /tmp/evidence/report.md <<EOF
**Command**:
streamlib run --dir examples/camera-display
EOF'
expect_ask "a heredoc quoting the launch command DOES escalate (residual false positive)"

# ── The class this key deliberately does NOT cover ───────────────────
# The ticket scopes the launch key to apps under examples/. `streamlib new`
# scaffolds an app anywhere, and running one boots a Runtime and a GPU context
# just the same — so these are unbraked by construction, not by accident.
# Locked here so the gap is visible and a decision to close it fails loudly.
run_hook 'streamlib run --dir /tmp/myapp'
expect_silent "a scaffolded app outside examples/ is NOT braked (scoped by the ticket)"

run_hook 'streamlib dev --dir /home/dev/scaffolds/camera-app'
expect_silent "streamlib dev outside examples/ is NOT braked (scoped by the ticket)"

# ── cargo run ────────────────────────────────────────────────────────
# examples/* are not workspace members, so no `-p` spelling resolves one. The
# example crates that remain are reached by cwd or manifest path.
run_hook 'cargo run -p camera-display'
expect_silent "cargo run -p camera-display names a crate that no longer exists"

run_hook 'cargo run -p jpeg-psnr'
expect_silent "no -p spelling reaches an example, so neither dead key survives"

# The text-tool guard covers both launch keys, not just the streamlib one.
run_hook 'git grep -n "cargo run" -- examples/'
expect_silent "grepping for cargo run in examples/ does not escalate"

run_hook 'git commit -m "docs: cargo run in examples/ still works"'
expect_silent "a commit message naming cargo run in examples/ does not escalate"

run_hook 'cargo run --release' '/home/dev/streamlib/examples/jpeg-psnr'
expect_ask "cargo run from inside an example directory escalates"

run_hook 'cargo run --manifest-path examples/jpeg-psnr/Cargo.toml'
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
