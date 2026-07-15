#!/usr/bin/env bash
# rig-brake (PreToolUse / Bash): refuse rig-consuming commands a sandboxed session
# cannot observe (they die at exit 144). Read-only device probes stay allowed.
#
# Every deny pattern is evaluated over the FULL command string, so a benign lead
# clause (e.g. a v4l2-ctl query verb) can never blanket-allow a rig command
# chained after it. The v4l2-ctl query verbs are exempt only from the v4l2
# streaming deny — they are not in that pattern, so they simply don't match it.
# Exit 2 + stderr = deny; exit 0 = allow.

input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // ""')"
cwd="$(printf '%s' "$input" | jq -r '.cwd // ""')"

deny_rig() {
  cat >&2 <<'MSG'
rig-brake: refused — sandboxed sessions cannot observe GPU/IPC runtime (exit 144).
Park the issue for /verify-live and post the command block for the owner's terminal.
MSG
  exit 2
}

has() { printf '%s' "$cmd" | grep -Eq -- "$1"; }
cwd_has() { printf '%s' "$cwd" | grep -Eq -- "$1"; }

# 1. ffmpeg with a v4l2 output or reading a camera device node.
if has '\bffmpeg\b' && has '\-f[[:space:]]+v4l2|/dev/video[0-9]+'; then
  deny_rig
fi

# 2. cargo run for example crates that open camera/display at runtime:
#    (a) the -p / --package matcher, (b) run from inside an examples/ dir
#    (payload cwd), or (c) the command references an examples/ path.
if has 'cargo[[:space:]]+run'; then
  if has '(-p|--package)[[:space:]]+(camera-display|vulkan-video-roundtrip)' \
     || cwd_has '(^|/)examples(/|$)' \
     || has '(^|[^[:alnum:]_.-])examples/'; then
    deny_rig
  fi
fi

# 3. e2e_ fixture scripts under tests/fixtures/.
if has 'tests/fixtures/e2e_[[:alnum:]_./-]*\.sh'; then
  deny_rig
fi

# 4. Reading/streaming a camera device node via a media tool — cat, dd, ffplay,
#    mpv, gst-launch*. A word-boundary match on the tool name plus a real
#    /dev/video[0-9] path. A bare textual mention (grep/echo/log path) does not
#    match: the tool name isn't one of these, or there's no /dev/video path.
if has '\b(cat|dd|ffplay|mpv|gst-launch(-[0-9.]+)?)\b' && has '/dev/video[0-9]+'; then
  deny_rig
fi

# 5. v4l2-ctl streaming verbs. Query verbs (--list-devices, --get-fmt-video,
#    --list-formats*, --all, --info, -D, --get-*) are exempt — they aren't in
#    this pattern, so a query-only v4l2-ctl command falls through to exit 0.
if has '\bv4l2-ctl\b' \
   && has '--stream-(mmap|user|to|out|dmabuf|from|dqmax)|--stream[[:space:]=]'; then
  deny_rig
fi

exit 0
