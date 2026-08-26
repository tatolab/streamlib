#!/usr/bin/env bash
# rig-brake (PreToolUse / Bash): a rig-consuming command (camera/display/GPU) that
# runs in an unattended/sandboxed firing dies at exit 144 — but when the owner is
# present it's a deliberate eval that SHOULD run. So rig-brake never has the final
# say: it ESCALATES a suspected rig command to the human for approval (permission
# "ask"), never hard-denies. The human always decides; benign probes/builds fall
# through silently, so it only asks when a command actually looks like it drives
# the rig.
#
# Contract: exit 0 + JSON `permissionDecision:"ask"` escalates to the human. Plain
# exit 0 (no output) defers to the normal permission flow. It never exits 2 — that
# would be a hard deny, the final say we deliberately don't take.

input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // ""')"
cwd="$(printf '%s' "$input" | jq -r '.cwd // ""')"

REASON='Looks like a rig-consuming command (camera/display/GPU). If you are here and this is a real eval, approve it — it runs with the sandbox bypass. If this is an unattended/sandboxed firing, decline and park it for /verify-live (it would otherwise die at exit 144). rig-brake only asks; you decide.'

ask_rig() {
  jq -n --arg r "$REASON" '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "ask", permissionDecisionReason: $r}}'
  exit 0
}

has() { printf '%s' "$cmd" | grep -Eq -- "$1"; }
cwd_has() { printf '%s' "$cwd" | grep -Eq -- "$1"; }

# 1. ffmpeg reading a camera device or writing a v4l2 output.
if has '\bffmpeg\b' && has '\-f[[:space:]]+v4l2|/dev/video[0-9]+'; then
  ask_rig
fi

# 2. Launching an example app, which opens a camera/display at runtime. Two
#    spellings: `streamlib run`/`dev` for the Python apps, `cargo run` for the
#    example crates still written in Rust. examples/* are not workspace members,
#    so neither spelling reaches one by `-p` from the repo root.
names_an_example() {
  cwd_has '(^|/)examples(/|$)' || has '(^|[^[:alnum:]_.-])examples/'
}

# A text tool that merely carries a launch command in an argument is not a
# launch. `git commit -m "… streamlib run --dir examples/x"`,
# `sed 's|streamlib run|…|' examples/README.md` and `git grep -n "cargo run"
# -- examples/` are the commands a session working on this hook writes
# constantly, and prompting on them is what teaches an owner to click through
# without reading.
#
# The filter is per line, not per command. `has` is `grep -Eq`, which is
# line-oriented, so testing the raw input let any line silence every other one
# — including the backgrounded launch + `echo $!` shape /verify-live itself
# prescribes. Dropping the text-tool lines and matching what survives keeps
# `git status && streamlib run …` silent (one line, led by git) while
# `echo starting` on a line above a real launch no longer hides it.
launch_candidate_lines() {
  printf '%s' "$cmd" \
    | grep -Ev '^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*(git|gh|sed|grep|rg|awk|perl|python3?|node|echo|printf|cat|jq|diff|rev|tee)([[:space:]]|$)'
}

matches() { printf '%s' "$2" | grep -Eq -- "$1"; }

# Both keys are anchored to a command position: start of a line or just past a
# separator, after any env assignments and any exec wrapper. `timeout` and
# `nohup` earn their place — bounding an unattended run is what a sandboxed
# firing does. The observation verbs (nodes/graph/tap/logs/exchange) match
# neither key; /verify-live runs them per frame and a prompt would stall it.
#
# Two shapes stay uncovered by construction. A `bash -c "…"` body is an
# unparsed string, so any rule reaching inside it also reaches inside
# `bash -c "grep -rn 'streamlib run' examples/"`. And a heredoc body line is
# indistinguishable from a command line — writing an evidence report that
# quotes the launch command will ask. Both cost one prompt or one exit 144,
# never a wrong result.
LAUNCH_AT_COMMAND_POSITION='(^|[;&|(])[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*((nohup|timeout|env|stdbuf|xvfb-run|uv|poetry)([[:space:]]+[^[:space:]]+)*[[:space:]]+)*([[:alnum:]_./-]*/)?streamlib[[:space:]]+(run|dev)([[:space:]]|$)'
CARGO_RUN_AT_COMMAND_POSITION='(^|[;&|(])[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*((nohup|timeout|env|stdbuf|xvfb-run)([[:space:]]+[^[:space:]]+)*[[:space:]]+)*cargo[[:space:]]+run([[:space:]]|$)'

if names_an_example; then
  candidates="$(launch_candidate_lines)"
  if matches "$LAUNCH_AT_COMMAND_POSITION" "$candidates" \
     || matches "$CARGO_RUN_AT_COMMAND_POSITION" "$candidates"; then
    ask_rig
  fi
fi

# 3. e2e_ fixture scripts under tests/fixtures/.
if has 'tests/fixtures/e2e_[[:alnum:]_./-]*\.sh'; then
  ask_rig
fi

# 4. A media PLAYER/streamer pointed at a real camera device node. cat/dd are
#    dropped from this rule: a benign `cat file` that merely mentions a device
#    path in the same command must not trigger an ask (the old false-positive).
if has '\b(ffplay|mpv|gst-launch(-[0-9.]+)?)\b' && has '/dev/video[0-9]+'; then
  ask_rig
fi

# 5. v4l2-ctl streaming verbs. Query verbs (--list-*, --get-*, --all, --info, -D)
#    aren't in this pattern, so a query-only v4l2-ctl command falls through.
if has '\bv4l2-ctl\b' \
   && has '--stream-(mmap|user|to|out|dmabuf|from|dqmax)|--stream[[:space:]=]'; then
  ask_rig
fi

exit 0
