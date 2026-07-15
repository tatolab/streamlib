#!/usr/bin/env bash
# rig-brake (PreToolUse / Bash): refuse rig-consuming commands a sandboxed session
# cannot observe (they die at exit 144). Read-only device probes stay allowed.
# Exit 2 + stderr = deny; exit 0 = allow.

input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // ""')"

deny_rig() {
  cat >&2 <<'MSG'
rig-brake: refused — sandboxed sessions cannot observe GPU/IPC runtime (exit 144).
Park the issue for /verify-live and post the command block for Jonathan's terminal.
MSG
  exit 2
}

match() { printf '%s' "$cmd" | grep -Eq "$1"; }

# v4l2-ctl: allow read-only query verbs (--list-devices, --get-fmt-video,
# --list-formats*, --all, --info, -D, --get-*); deny streaming verbs.
if match '(^|[^[:alnum:]_-])v4l2-ctl([^[:alnum:]_-]|$)'; then
  if printf '%s' "$cmd" | grep -Eq -- '--stream-(mmap|user|to|out|dmabuf|from|dqmax)|--stream[[:space:]=]'; then
    deny_rig
  fi
  exit 0
fi

# ffmpeg with a v4l2 output or reading a camera device node.
if match '(^|[^[:alnum:]_-])ffmpeg([^[:alnum:]_-]|$)'; then
  if printf '%s' "$cmd" | grep -Eq -- '-f[[:space:]]+v4l2|/dev/video[0-9]+'; then
    deny_rig
  fi
fi

# cargo run for example crates that open camera/display at runtime.
if match 'cargo[[:space:]]+run'; then
  if printf '%s' "$cmd" | grep -Eq -- '(-p|--package)[[:space:]]+(camera-display|vulkan-video-roundtrip)'; then
    deny_rig
  fi
fi

# e2e_ fixture scripts under tests/fixtures/.
if match 'tests/fixtures/e2e_[[:alnum:]_./-]*\.sh'; then
  deny_rig
fi

exit 0
