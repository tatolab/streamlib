#!/usr/bin/env bash
# Unit tests for .claude/hooks/rig-brake.sh and .claude/scripts/rig-brake.
#
# Three outcomes matter and they pull against each other. A rig command must be
# noticed: a note by default, a prompt only where the owner configured one. A
# benign command that merely mentions a launch, a fixture path or a device must
# stay silent, because noise teaches the owner to click through and the model to
# ignore the note. And the owner's config must win in the documented precedence.
#
# No toolchain, no network: bash + jq. HOME and CLAUDE_PROJECT_DIR point at a
# scratch directory so the machine's real settings never leak into a case.
set -uo pipefail

claude_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
hook="$claude_dir/hooks/rig-brake.sh"
helper="$claude_dir/scripts/rig-brake"
[ -f "$hook" ] || { echo "hook not found at $hook" >&2; exit 2; }
[ -f "$helper" ] || { echo "helper not found at $helper" >&2; exit 2; }
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 2; }

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
export HOME="$scratch/home"
project="$scratch/project"
mkdir -p "$HOME/.claude" "$project/.claude"

passed=0
failed=0
out=""
status=0

# run_hook <command> [cwd] — one synthetic PreToolUse tool input, the JSON shape
# Claude Code hands the hook on a Bash call.
run_hook() {
  out="$(jq -n --arg c "$1" --arg d "${2:-/home/dev/streamlib}" \
    '{tool_input: {command: $c}, cwd: $d}' | CLAUDE_PROJECT_DIR="$project" bash "$hook" 2>&1)"
  status=$?
}

run_helper() {
  out="$(CLAUDE_PROJECT_DIR="$project" bash "$helper" "$@" 2>&1)"
  status=$?
}

user_config="$HOME/.claude/rig-brake.json"
project_config="$project/.claude/rig-brake.json"
local_config="$project/.claude/rig-brake.local.json"

# set_config <user|project|local> '<json>'
set_config() {
  case "$1" in
    user) printf '%s\n' "$2" >"$user_config" ;;
    project) printf '%s\n' "$2" >"$project_config" ;;
    local) printf '%s\n' "$2" >"$local_config" ;;
  esac
}
clear_config() { rm -f "$user_config" "$project_config" "$local_config"; }

ok() { passed=$((passed + 1)); printf '  ok   %s\n' "$1"; }
bad() {
  failed=$((failed + 1))
  printf '  FAIL %s\n' "$1"
  printf '%s\n' "$out" | sed 's/^/       | /'
}

field() { printf '%s' "$out" | jq -r "$1 // \"\"" 2>/dev/null; }

# A note: exit 0, a systemMessage for the owner, additionalContext for the model,
# and no permissionDecision at all. The optional second argument must appear in
# the model's note, which is how a case pins the rule that fired.
expect_warn() {
  local context message decision
  context="$(field '.hookSpecificOutput.additionalContext')"
  message="$(field '.systemMessage')"
  decision="$(field '.hookSpecificOutput.permissionDecision')"
  if [ "$status" -eq 0 ] && [ -n "$context" ] && [ -n "$message" ] && [ -z "$decision" ] \
     && { [ -z "${2:-}" ] || [[ "$context" == *"$2"* ]]; }; then
    ok "$1"
  else
    bad "$1 (expected exit 0 + a note${2:+ naming $2}, got exit $status decision '${decision:-none}')"
  fi
}

# A prompt: exit 0 carrying permissionDecision "ask". Never exit 2. The optional
# second argument must appear in the reason shown to the owner.
expect_ask() {
  local decision reason
  decision="$(field '.hookSpecificOutput.permissionDecision')"
  reason="$(field '.hookSpecificOutput.permissionDecisionReason')"
  if [ "$status" -eq 0 ] && [ "$decision" = "ask" ] \
     && { [ -z "${2:-}" ] || [[ "$reason" == *"$2"* ]]; }; then
    ok "$1"
  else
    bad "$1 (expected exit 0 + ask${2:+ mentioning $2}, got exit $status decision '${decision:-none}')"
  fi
}

# Silence must be total: any stdout at all is a note or a decision.
expect_silent() {
  if [ "$status" -eq 0 ] && [ -z "$out" ]; then
    ok "$1"
  else
    bad "$1 (expected exit 0 + no output, got exit $status)"
  fi
}

expect_status() {
  if [ "$status" -eq "$2" ] && { [ -z "${3:-}" ] || [[ "$out" == *"$3"* ]]; }; then
    ok "$1"
  else
    bad "$1 (expected exit $2${3:+ mentioning $3}, got exit $status)"
  fi
}

echo "rig-brake.sh — default config: every rule notes, nothing prompts"
clear_config

# ── The Python launch path ───────────────────────────────────────────
run_hook 'streamlib run --dir examples/camera-display'
expect_warn "streamlib run of an app under examples/ is noted" example_launch

run_hook 'streamlib dev --dir examples/camera-python-effects'
expect_warn "streamlib dev of an app under examples/ is noted" example_launch

run_hook 'STREAMLIB_CAMERA_DEVICE=/dev/video0 streamlib run --dir examples/camera-display'
expect_warn "an env-prefixed streamlib run is noted" example_launch

run_hook '/home/dev/streamlib/sdk/streamlib-python-wheel/.venv/bin/streamlib run --dir /home/dev/streamlib/examples/camera-display'
expect_warn "the wheel venv CLI named by absolute path is noted" example_launch

run_hook 'streamlib run' '/home/dev/streamlib/examples/camera-display'
expect_warn "streamlib run from inside an example directory is noted" example_launch

# ── The observation verbs stay silent ────────────────────────────────
# /verify-live taps a channel and exchanges surface ids per frame; a note on
# each read would bury the audit in reminders.
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

# ── Prose mentioning the launch path is not the launch path ──────────
run_hook 'grep -rn "streamlib run" examples/ .claude/'
expect_silent "grepping for the words streamlib run stays silent"

run_hook 'git commit -m "fix(cli): streamlib run --dir examples/foo now resolves app.py"'
expect_silent "a commit message quoting the launch path stays silent"

run_hook 'gh pr create --title docs --body "run it with streamlib run --dir examples/x"'
expect_silent "a PR body quoting the launch path stays silent"

run_hook 'sed -i "s|streamlib run |streamlib dev |" examples/camera-display/README.md'
expect_silent "rewriting the launch path in a doc stays silent"

# A compound whose FIRST word is a text tool hides a launch behind it. Rare, and
# the miss costs a note, where the false positives above are frequent.
run_hook 'git status && streamlib run --dir examples/camera-display'
expect_silent "a launch hidden behind a leading text tool is NOT noted (accepted trade-off)"

run_hook 'cd examples/camera-display && streamlib run'
expect_warn "a compound not led by a text tool is noted" example_launch

# Only the first word of a LINE may suppress; a text-tool line above or below a
# launch does not hide it.
run_hook 'streamlib run --dir examples/camera-display > /tmp/rig.log 2>&1 &
echo "pid $!"'
expect_warn "a backgrounded launch followed by echo is noted" example_launch

run_hook 'cd examples/camera-display
streamlib run > /tmp/rig.log 2>&1 &
echo started'
expect_warn "a multi-line cd + launch + echo is noted" example_launch

run_hook 'STREAMLIB_CAMERA_DEVICE=/dev/video0 streamlib run --dir examples/camera-display > /tmp/rig.log 2>&1 &
echo "launched $!"'
expect_warn "the skill's own prescribed launch shape is noted" example_launch

run_hook 'echo starting
streamlib run --dir examples/camera-display &'
expect_warn "an echo preamble does not hide the launch beneath it" example_launch

run_hook 'grep -q vivid /proc/modules
streamlib run --dir examples/camera-display &'
expect_warn "probing the rig then launching is noted" example_launch

# Exec wrappers: bounding an unattended run is what a sandboxed firing does.
run_hook 'nohup streamlib run --dir examples/camera-display &'
expect_warn "a launch behind nohup is noted" example_launch

run_hook 'timeout 30 streamlib run --dir examples/camera-display'
expect_warn "a launch behind timeout is noted" example_launch

run_hook 'timeout --kill-after=5 30 streamlib run --dir examples/camera-display'
expect_warn "a launch behind timeout with flags is noted" example_launch

run_hook 'uv run streamlib run --dir examples/camera-display'
expect_warn "a launch behind uv run is noted" example_launch

run_hook 'DISPLAY=:1 STREAMLIB_CAMERA_DEVICE=/dev/video0 nohup streamlib run --dir examples/camera-display >/tmp/log 2>&1 &'
expect_warn "env assignments plus a wrapper plus redirection is noted" example_launch

run_hook "python3 -c \"print('streamlib run --dir examples/x')\""
expect_silent "printing the launch path from a script is not launching it"

run_hook 'curl -sX POST -d "streamlib run --dir examples/x" http://localhost:9000/notes'
expect_silent "posting the launch path as data is not launching it"

run_hook 'ls examples/ && grep -rn "cargo run" examples/'
expect_silent "listing then grepping for cargo run stays silent"

run_hook 'tail -20 examples/jpeg-psnr/README.md'
expect_silent "reading an example README stays silent"

# ── Not a launch: --help, and cargo run of a workspace target ────────
run_hook '.venv/bin/streamlib run --help 2>&1 | head -25' '/home/dev/streamlib/examples/audio-mixer-demo'
expect_silent "streamlib run --help prints and exits"

run_hook 'streamlib run -h' '/home/dev/streamlib/examples/audio-mixer-demo'
expect_silent "streamlib run -h prints and exits"

run_hook 'cd /home/dev/streamlib && cargo run -q -p xtask -- check-all-source-gates 2>&1 | grep -v examples/'
expect_silent "cargo run -p xtask beside an examples/ mention is a workspace tool, not a launch"

run_hook 'cargo run -p xtask -- check-no-in-process-placement'
expect_silent "cargo run of a workspace tool stays silent"

run_hook 'cargo run -p camera-display'
expect_silent "no -p spelling reaches an example crate"

run_hook 'cargo run -p streamlib-engine --example codec_roundtrip_rig -- --source camera'
expect_silent "the codec rig is a workspace target and names no examples/ path (known gap, locked)"

run_hook 'cargo run --release' '/home/dev/streamlib/examples/jpeg-psnr'
expect_warn "cargo run from inside an example directory is noted" example_launch

run_hook 'cargo run --manifest-path examples/jpeg-psnr/Cargo.toml'
expect_warn "cargo run against an example manifest is noted" example_launch

run_hook 'git grep -n "cargo run" -- examples/'
expect_silent "grepping for cargo run in examples/ stays silent"

# ── Heredoc bodies are data ──────────────────────────────────────────
run_hook 'cat > examples/camera-codec-roundtrip/README.md <<'"'"'MD'"'"'
# camera-codec-roundtrip

Run it:

streamlib run --dir examples/camera-codec-roundtrip
MD'
expect_silent "a README heredoc quoting the launch stays silent"

run_hook 'cat > app.py <<'"'"'PY'"'"'
"""Run with:
streamlib run --dir .
"""
PY' '/home/dev/streamlib/examples/camera-halftone'
expect_silent "a docstring heredoc quoting the launch stays silent"

run_hook 'cat > /tmp/evidence/report.md <<EOF
**Command**:
streamlib run --dir examples/camera-display
EOF'
expect_silent "an evidence report heredoc quoting the launch stays silent"

run_hook 'git commit -q -m "$(cat <<'"'"'EOF'"'"'
feat(examples): camera-codec-roundtrip

streamlib run --dir examples/camera-codec-roundtrip
EOF
)"'
expect_silent "a commit-message heredoc quoting the launch stays silent"

run_hook 'cat <<'"'"'EOF'"'"' > /tmp/x
cargo run
EOF' '/home/dev/streamlib/examples/jpeg-psnr'
expect_silent "a heredoc quoting cargo run inside an example directory stays silent"

run_hook 'cat > /tmp/notes.md <<'"'"'EOF'"'"'
see examples/
EOF
streamlib run --dir examples/camera-display'
expect_warn "a heredoc above a launch does not hide the launch" example_launch

# ── Known-uncovered shapes, locked so a change to them fails loudly ──
run_hook 'bash -c "streamlib run --dir examples/camera-display"'
expect_silent "a launch inside bash -c is NOT noted (unparsed string body)"

run_hook 'streamlib run --dir /tmp/myapp'
expect_silent "a scaffolded app outside examples/ is NOT noted (scoped to examples/)"

run_hook 'streamlib dev --dir /home/dev/scaffolds/camera-app'
expect_silent "streamlib dev outside examples/ is NOT noted (scoped to examples/)"

# ── e2e fixture scripts: executing one is noted, naming one is not ───
run_hook 'runtime/streamlib-engine/tests/fixtures/e2e_camera_display.sh /tmp/streamlib-e2e'
expect_warn "an e2e fixture script run directly is noted" e2e_fixture

run_hook './runtime/streamlib-engine/tests/fixtures/e2e_audio_loopback.sh /tmp/va'
expect_warn "an e2e fixture script run by relative path is noted" e2e_fixture

run_hook 'DISPLAY=:1 runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh /tmp/psnr-full h264 2>&1 | tail -45'
expect_warn "an env-prefixed e2e fixture run is noted" e2e_fixture

run_hook 'DISPLAY=:1 REFERENCE_STEMS="solid_red complex_pattern" SAMPLES_PER_REFERENCE=2 runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh /tmp/psnr-v h264'
expect_warn "a quoted env value with a space does not hide the run" e2e_fixture

run_hook 'timeout 2400 bash runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh /tmp/vivid-h264 h264 2>&1 | grep -E "RESULT"'
expect_warn "an e2e fixture run behind timeout and bash is noted" e2e_fixture

run_hook 'cd /home/dev/streamlib
export DISPLAY=:1
RUN_SECONDS=30 timeout 570 bash runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh /tmp/x h264'
expect_warn "a multi-line cd + export + e2e run is noted" e2e_fixture

run_hook 'for codec in h264 h265; do
  echo "######## $codec"
  REFERENCE_STEMS="complex_pattern" timeout 2400 bash runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh /tmp/psnr-$codec $codec
done'
expect_warn "an e2e run inside a for loop is noted" e2e_fixture

run_hook 'PYTHON=sdk/streamlib-python-wheel/.venv/bin/python \
  ./runtime/streamlib-engine/tests/fixtures/e2e_audio_loopback.sh /tmp/va-healthy'
expect_warn "an e2e run on a continuation line is noted" e2e_fixture

run_hook "sed -n '1,120p' runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh"
expect_silent "reading an e2e fixture script with sed stays silent"

run_hook 'cat -n runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh'
expect_silent "reading an e2e fixture script with cat stays silent"

run_hook "git diff runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh | sed -n '1,200p'"
expect_silent "diffing an e2e fixture script stays silent"

run_hook 'chmod +x runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh'
expect_silent "chmod on an e2e fixture script stays silent"

run_hook 'bash -n runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh && echo "syntax OK"'
expect_silent "a bash -n syntax check of an e2e fixture script stays silent"

run_hook 'git add runtime/streamlib-engine/tests/fixtures/codec_roundtrip_node.py runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh'
expect_silent "staging an e2e fixture script stays silent"

run_hook 'grep -n "STREAMLIB_CLI" runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh | head'
expect_silent "grepping an e2e fixture script stays silent"

run_hook 'ls runtime/streamlib-engine/tests/fixtures/ && head -40 runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh'
expect_silent "listing then reading an e2e fixture script stays silent"

run_hook 'python3 - <<'"'"'PY'"'"'
import pathlib
p = pathlib.Path("runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh")
s = p.read_text()
PY'
expect_silent "a python heredoc editing an e2e fixture script stays silent"

run_hook 'cat > /tmp/pr-body.md <<'"'"'MD'"'"'
## Summary

The fixture runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh gains a Python arm.
MD'
expect_silent "a PR body naming an e2e fixture script stays silent"

# ── Device rules ─────────────────────────────────────────────────────
run_hook 'ffmpeg -f v4l2 -i /dev/video10 -t 5 out.mp4'
expect_warn "ffmpeg reading a camera device is noted" ffmpeg_v4l2

run_hook 'git commit -m "ffmpeg -f v4l2 -i /dev/video0 is the capture path"'
expect_silent "a commit message quoting an ffmpeg capture stays silent"

run_hook 'ffplay /dev/video0'
expect_warn "ffplay pointed at a device node is noted" player_on_device

run_hook 'mpv /dev/video2'
expect_warn "a media player pointed at a device node is noted" player_on_device

run_hook 'mpv --version'
expect_silent "a media player not pointed at a device stays silent"

run_hook 'v4l2-ctl -d /dev/video0 --stream-mmap --stream-count=10'
expect_warn "a v4l2-ctl streaming verb is noted" v4l2ctl_stream

run_hook 'v4l2-ctl --list-devices'
expect_silent "a v4l2-ctl query verb stays silent"

run_hook 'v4l2-ctl -d /dev/video0 --get-fmt-video'
expect_silent "probing a device format stays silent"

run_hook 'cat /tmp/notes-about-/dev/video0.txt'
expect_silent "a benign command that merely mentions a device path stays silent"

run_hook 'cargo build -p streamlib-engine'
expect_silent "an ordinary build stays silent"

run_hook 'cd examples/camera-display && ffmpeg -f v4l2 -i /dev/video0 -t 2 /tmp/a.mp4 && streamlib run'
expect_warn "a command matching two rules names both in the note" ffmpeg_v4l2
[[ "$(field '.hookSpecificOutput.additionalContext')" == *example_launch* ]] \
  && ok "the second matched rule is named too" || bad "the second matched rule is missing from the note"

echo ""
echo "rig-brake.sh — what the owner and the model are told"
run_hook 'streamlib run --dir examples/camera-display'
[[ "$(field '.systemMessage')" == *"rig-brake rule example_launch off"* ]] \
  && ok "the owner's line names the rule switch" || bad "the owner's line does not name the rule switch"
[[ "$(field '.systemMessage')" == *"rig-brake allow '*streamlib run*'"* ]] \
  && ok "the owner's line offers a glob cut after the key" || bad "the owner's line does not offer the glob"
[[ "$(field '.hookSpecificOutput.additionalContext')" == *"never add an exception yourself"* ]] \
  && ok "the model is told not to add exceptions on its own" || bad "the model's note lacks the no-self-exception line"
[[ "$(field '.hookSpecificOutput.additionalContext')" == *"exit 144"* ]] \
  && ok "the model is told about exit 144" || bad "the model's note lacks the exit 144 warning"

run_hook 'DISPLAY=:1 runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh /tmp/v h264'
[[ "$(field '.systemMessage')" == *"allow '*DISPLAY=:1 runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh*'"* ]] \
  && ok "the suggested glob keeps the line up to the script name" || bad "the suggested glob is not cut after the script name"

set_config project '{"preferences": "Ask me before touching /dev/video2."}'
run_hook 'streamlib run --dir examples/camera-display'
expect_warn "owner preferences reach the model's note" "Ask me before touching /dev/video2."
clear_config

echo ""
echo "rig-brake.sh — outcomes from config"
set_config project '{"mode": "ask"}'
run_hook 'streamlib run --dir examples/camera-display'
expect_ask "mode ask turns a note into a prompt" "rig-brake rule example_launch warn"
run_hook 'cargo build -p streamlib-engine'
expect_silent "mode ask does not touch a command no rule matches"
clear_config

set_config project '{"rules": {"example_launch": "ask"}}'
run_hook 'streamlib run --dir examples/camera-display'
expect_ask "a per-rule ask prompts on that rule" "rules.example_launch"
run_hook 'runtime/streamlib-engine/tests/fixtures/e2e_camera_display.sh /tmp/e2e'
expect_warn "a per-rule ask leaves the other rules at warn" e2e_fixture
clear_config

set_config project '{"rules": {"example_launch": "off"}}'
run_hook 'streamlib run --dir examples/camera-display'
expect_silent "a per-rule off silences that rule"
run_hook 'ffmpeg -f v4l2 -i /dev/video10 -t 5 out.mp4'
expect_warn "a per-rule off leaves the other rules at warn" ffmpeg_v4l2
clear_config

set_config project '{"rules": {"e2e_fixture": "off"}}'
run_hook 'DISPLAY=:1 runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh /tmp/v h264'
expect_silent "e2e_fixture off silences a fixture run"
clear_config

set_config project '{"mode": "off"}'
run_hook 'cd examples/camera-display && ffmpeg -f v4l2 -i /dev/video0 -t 2 /tmp/a.mp4 && streamlib run'
expect_silent "mode off silences every rule"
clear_config

set_config project '{"rules": {"ffmpeg_v4l2": "ask"}}'
run_hook 'cd examples/camera-display && ffmpeg -f v4l2 -i /dev/video0 -t 2 /tmp/a.mp4 && streamlib run'
expect_ask "when two rules match, an ask on either prompts" "rules.ffmpeg_v4l2"
clear_config

echo ""
echo "rig-brake.sh — globs"
set_config project '{"allow": ["*e2e_fixture_psnr_vivid.sh*"]}'
run_hook 'DISPLAY=:1 runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr_vivid.sh /tmp/v h264'
expect_silent "an allow glob silences a matching command"
run_hook 'DISPLAY=:1 runtime/streamlib-engine/tests/fixtures/e2e_fixture_psnr.sh /tmp/p h264'
expect_warn "an allow glob leaves a non-matching command noted" e2e_fixture
clear_config

set_config project '{"ask": ["cargo build*"]}'
run_hook 'cargo build -p streamlib-engine'
expect_ask "an ask glob prompts even where no rule matches" "ask glob"
run_hook 'cargo build -p streamlib-engine'
expect_ask "an ask glob's prompt says how to remove it" "rig-brake remove 'cargo build*'"
run_hook 'cargo test -p streamlib-engine'
expect_silent "an ask glob leaves a non-matching command alone"
clear_config

set_config project '{"allow": ["*streamlib run*"], "ask": ["*streamlib run*"]}'
run_hook 'streamlib run --dir examples/camera-display'
expect_ask "an ask glob beats an allow glob"
clear_config

set_config project '{"rules": {"example_launch": "ask"}, "allow": ["*streamlib run*"]}'
run_hook 'streamlib run --dir examples/camera-display'
expect_silent "an allow glob beats a per-rule ask"
clear_config

set_config project '{"allow": ["*camera-display*"]}'
run_hook 'streamlib run --dir examples/camera-display
echo "pid $!"'
expect_silent "a glob matches across the whole multi-line command"
clear_config

echo ""
echo "rig-brake.sh — scopes"
set_config user '{"mode": "ask"}'
set_config project '{"mode": "warn"}'
run_hook 'streamlib run --dir examples/camera-display'
expect_warn "project mode wins over user mode" example_launch
clear_config

set_config project '{"mode": "warn"}'
set_config local '{"rules": {"example_launch": "off"}}'
run_hook 'streamlib run --dir examples/camera-display'
expect_silent "a local rule wins over project mode"
clear_config

set_config project '{"rules": {"example_launch": "off"}}'
set_config local '{"rules": {"example_launch": "ask"}}'
run_hook 'streamlib run --dir examples/camera-display'
expect_ask "a local rule wins over a project rule"
clear_config

set_config user '{"allow": ["*camera-display*"]}'
set_config local '{"allow": ["*ffmpeg*"]}'
run_hook 'streamlib run --dir examples/camera-display'
expect_silent "a user allow glob still applies beside a local one"
run_hook 'ffmpeg -f v4l2 -i /dev/video10 -t 5 out.mp4'
expect_silent "a local allow glob applies beside a user one"
run_hook 'mpv /dev/video2'
expect_warn "allow globs from every scope leave the rest noted" player_on_device
clear_config

echo ""
echo "rig-brake.sh — broken config degrades to a note, never a prompt"
printf '{"mode": {\n' >"$local_config"
run_hook 'streamlib run --dir examples/camera-display'
expect_warn "invalid JSON in one file still notes" example_launch
[[ "$(field '.systemMessage')" == *"rig-brake.local.json is not valid JSON"* ]] \
  && ok "the owner's line names the broken file" || bad "the owner's line does not name the broken file"
clear_config

set_config project '{"mode": "loud", "rules": {"bogus": "ask", "example_launch": "maybe"}}'
run_hook 'streamlib run --dir examples/camera-display'
expect_warn "unknown outcomes and rules fall back to warn" example_launch
[[ "$(field '.systemMessage')" == *"Ignored rig-brake config entries"* && "$(field '.systemMessage')" == *"rules.bogus"* ]] \
  && ok "the owner's line lists the ignored entries" || bad "the owner's line does not list the ignored entries"
clear_config

set_config project '{"mode": 7, "rules": "none", "allow": "x", "ask": [1, "cargo build*"]}'
run_hook 'cargo build'
expect_ask "wrong-typed entries are skipped without losing the valid ones"
clear_config

run_hook ''
expect_silent "an empty command stays silent"

echo ""
echo "rig-brake.sh --show-config"
out="$(CLAUDE_PROJECT_DIR="$project" bash "$hook" --show-config 2>&1)"; status=$?
[ "$status" -eq 0 ] && [ "$(field '.config.mode')" = "warn" ] \
  && ok "no config prints the warn default" || bad "--show-config without config"
[ "$(printf '%s' "$out" | jq '.sources | length')" = "3" ] \
  && ok "the three config paths are listed as sources" || bad "sources are not the three config paths"
set_config project '{"mode": "off", "allow": ["a*"]}'
set_config local '{"allow": ["b*"]}'
out="$(CLAUDE_PROJECT_DIR="$project" bash "$hook" --show-config 2>&1)"; status=$?
[ "$(field '.config.mode')" = "off" ] && [ "$(printf '%s' "$out" | jq -c '.config.allow')" = '["a*","b*"]' ] \
  && ok "the merged config is printed" || bad "the merged config is wrong"
clear_config

echo ""
echo "scripts/rig-brake"
run_helper allow 'cargo run -p xtask *'
expect_status "allow writes the local config file" 0 "rig-brake.local.json"
[ "$(jq -c '.allow' "$local_config")" = '["cargo run -p xtask *"]' ] \
  && ok "the glob is stored under allow" || bad "the glob is not stored"
run_helper allow 'cargo run -p xtask *'
[ "$(jq '.allow | length' "$local_config")" = "1" ] \
  && ok "adding the same glob twice stores it once" || bad "the glob was duplicated"

printf '{"preferences": "keep me"}\n' >"$project_config"
run_helper rule e2e_fixture off --project
expect_status "rule --project writes the checked-in config file" 0 "$project_config"
[ "$(jq -r '.rules.e2e_fixture' "$project_config")" = "off" ] \
  && ok "the rule outcome is stored" || bad "the rule outcome is not stored"
[ "$(jq -r '.preferences' "$project_config")" = "keep me" ] \
  && ok "other keys survive the edit" || bad "other keys were lost"

run_helper mode ask --user
expect_status "mode --user writes the user config file" 0 "$user_config"
[ "$(jq -r '.mode' "$user_config")" = "ask" ] \
  && ok "the mode is stored" || bad "the mode is not stored"

run_helper ask '*ffmpeg*'
[ "$(jq -c '.ask' "$local_config")" = '["*ffmpeg*"]' ] \
  && ok "ask stores the glob under ask" || bad "the ask glob is not stored"
run_helper remove '*ffmpeg*'
[ "$(jq -c '.ask' "$local_config")" = '[]' ] \
  && ok "remove drops the glob" || bad "remove left the glob in place"

run_helper rule nope off
expect_status "an unknown rule is refused" 1 "unknown rule"
run_helper rule e2e_fixture loud
expect_status "an unknown outcome is refused" 1 "warn, ask or off"
run_helper frobnicate
expect_status "an unknown verb is refused" 1 "unknown verb"

run_helper show
expect_status "show prints the effective config" 0 '"mode": "ask"'
[[ "$out" == *"cargo run -p xtask *"* ]] \
  && ok "show includes the local allow glob" || bad "show is missing the local allow glob"
[[ "$out" == *"== $user_config =="* && "$out" == *"== $local_config =="* ]] \
  && ok "show lists every source file" || bad "show does not list every source file"

clear_config
run_helper test 'streamlib run --dir examples/camera-display'
expect_status "test reports a note" 0 "outcome: warn"
run_helper test 'cargo build'
expect_status "test reports silence" 0 "outcome: silent"
set_config project '{"mode": "ask"}'
run_helper test 'streamlib run --dir examples/camera-display'
expect_status "test reports a prompt" 0 "outcome: ask"
run_helper test --cwd /home/dev/streamlib/examples/camera-display 'cargo run --release'
expect_status "test honours --cwd" 0 "outcome: ask"
clear_config

printf '{"mode": {\n' >"$local_config"
run_helper allow 'x*'
expect_status "a broken target file is refused rather than clobbered" 1 "not valid JSON"
[ "$(cat "$local_config")" = '{"mode": {' ] \
  && ok "the broken file is left as it was" || bad "the broken file was changed"
clear_config

echo ""
printf '  %d passed, %d failed\n' "$passed" "$failed"
[ "$failed" -eq 0 ]
