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
# launch. `git commit -m "… streamlib run --dir examples/x"` and
# `sed 's|streamlib run|…|' examples/README.md` are the commands a session
# working on this hook writes constantly, and prompting on them is what teaches
# an owner to click through without reading.
#
# Only the FIRST command word decides. `has` is `grep -Eq`, which anchors `^`
# per line, so testing the raw input would let any line of a multi-line command
# suppress the whole thing — including the backgrounded launch + `echo $!` shape
# /verify-live itself prescribes.
first_command_word_is_a_text_tool() {
  printf '%s' "$cmd" \
    | sed -n '/[^[:space:]]/{p;q}' \
    | grep -Eq '^[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*(git|gh|sed|grep|rg|awk|perl|python3?|node|echo|printf|cat|jq|diff|rev|tee)([[:space:]]|$)'
}

# The binary may sit behind env assignments and behind an exec wrapper —
# `timeout` and `nohup` especially, since bounding an unattended run is exactly
# what a sandboxed firing does. `bash -c "…"` stays uncovered: the quote that
# makes it a launch is the same quote that makes `python3 -c "print(…)"` text.
LAUNCH_AT_COMMAND_POSITION='(^|[;&|(])[[:space:]]*([A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+)*((nohup|timeout|env|stdbuf|xvfb-run|uv|poetry)([[:space:]]+[^[:space:]]+)*[[:space:]]+)*([[:alnum:]_./-]*/)?streamlib[[:space:]]+(run|dev)([[:space:]]|$)'

# Both launch spellings share the guard: `git grep -n "cargo run" -- examples/`
# is the same false positive as its streamlib twin. The observation verbs
# (nodes/graph/tap/logs/exchange) are control-plane reads and match neither key
# — /verify-live runs them per frame, and a prompt there would stall the audit.
if ! first_command_word_is_a_text_tool && names_an_example; then
  if has "$LAUNCH_AT_COMMAND_POSITION" || has 'cargo[[:space:]]+run'; then
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
