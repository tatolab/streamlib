#!/usr/bin/env bash
# rig-brake (PreToolUse / Bash): notes a rig-consuming Bash command (camera / display /
# GPU) to the model and to the owner. Advisory by default: every rule's outcome is `warn`
# unless the owner sets otherwise, and it never denies.
#
# Config contract — ~/.claude/rig-brake.json, .claude/rig-brake.json and
# .claude/rig-brake.local.json, read in that order. Later files win on `mode`, `rules` and
# `preferences`; `allow` and `ask` lists concatenate. Claude Code's settings validator
# rejects unknown keys in settings.json, so the config lives beside it, not inside it.
#
#   {
#     "mode": "warn",                              warn | ask | off — the default for every rule
#     "rules": { "e2e_fixture": "off" },           one rule's outcome
#     "allow": [ "*e2e_fixture_psnr_vivid.sh*" ],  globs on the whole command text: stay silent
#     "ask":   [ "ffmpeg * -f v4l2 /dev/video*" ], globs on the whole command text: prompt
#     "preferences": "text appended to the note the model reads"
#   }
#
# Precedence: an `ask` glob, then an `allow` glob, then the rule's own outcome, then `mode`.
# `.claude/scripts/rig-brake` edits and dry-runs this config; `--show-config` prints it.
#
# Output contract: warn = exit 0 + JSON carrying `systemMessage` (owner) and
# `hookSpecificOutput.additionalContext` (model), no permissionDecision; ask = exit 0 +
# `permissionDecision:"ask"`; off or no match = exit 0 with no output. Never exit 2.
set -o pipefail

RULE_NAMES='ffmpeg_v4l2 example_launch e2e_fixture player_on_device v4l2ctl_stream'
HELPER='.claude/scripts/rig-brake'
root="${CLAUDE_PROJECT_DIR:-$(pwd)}"

# ── config ───────────────────────────────────────────────────────────
config_files=("$HOME/.claude/rig-brake.json" "$root/.claude/rig-brake.json" "$root/.claude/rig-brake.local.json")
config_problems=""
fragments=('{}')
for config_file in "${config_files[@]}"; do
  [ -f "$config_file" ] || continue
  if fragment="$(jq -c '.' "$config_file" 2>/dev/null)"; then
    fragments+=("$fragment")
  else
    config_problems="${config_problems}${config_file} is not valid JSON, so it is ignored. "
  fi
done
config="$(printf '%s\n' "${fragments[@]}" | jq -s --arg names "$RULE_NAMES" '
  def outcome: type == "string" and (. as $v | ["warn", "ask", "off"] | index($v) != null);
  def object_or_empty: if type == "object" then . else {} end;
  def strings_or_empty: if type == "array" then map(strings) else [] end;
  ($names | split(" ")) as $known
  | reduce (.[] | object_or_empty) as $c (
      {mode: "warn", rules: {}, allow: [], ask: [], preferences: "", invalid: []};
      .mode = (if ($c.mode | outcome) then $c.mode else .mode end)
      | .invalid += (if $c.mode != null and ($c.mode | outcome | not) then ["mode=\($c.mode | tojson)"] else [] end)
      | ($c.rules | object_or_empty) as $rules
      | .rules += ($rules | with_entries(select((.key | IN($known[])) and (.value | outcome))))
      | .invalid += ($rules | to_entries
                     | map(select((.key | IN($known[]) | not) or (.value | outcome | not))
                           | "rules.\(.key)=\(.value | tojson)"))
      | .allow += ($c.allow | strings_or_empty)
      | .ask += ($c.ask | strings_or_empty)
      | .preferences = (if ($c.preferences | type) == "string" then $c.preferences else .preferences end))
')"
invalid_entries="$(jq -r '.invalid | join(", ")' <<<"$config")"
[ -z "$invalid_entries" ] \
  || config_problems="${config_problems}Ignored rig-brake config entries ${invalid_entries} (outcomes are warn, ask or off; rules are ${RULE_NAMES// /, }). "

if [ "${1:-}" = "--show-config" ]; then
  jq --arg problems "$config_problems" --args \
    '{config: del(.invalid), sources: $ARGS.positional, problems: $problems}' \
    "${config_files[@]}" <<<"$config"
  exit 0
fi

# ── the command ──────────────────────────────────────────────────────
input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // ""')"
cwd="$(printf '%s' "$input" | jq -r '.cwd // ""')"
[ -n "$cmd" ] || exit 0

# A heredoc body is data, not a command line: a README that quotes the launch, a
# docstring, a PR body. Only the lines outside heredoc bodies are matched.
read -r -d '' STRIP_HEREDOC_BODIES <<'AWK' || true
in_body {
  line = $0
  sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line)
  if (line == tag) in_body = 0
  next
}
{ print }
match($0, /(^|[^<])<<-?[[:space:]]*["']?[A-Za-z_][A-Za-z0-9_]*/) {
  tag = substr($0, RSTART, RLENGTH)
  sub(/^.?<<-?[[:space:]]*["']?/, "", tag)
  in_body = 1
}
AWK
stripped="$(printf '%s\n' "$cmd" | awk "$STRIP_HEREDOC_BODIES")"

# A line led by a text tool carries a launch as an argument, never as a launch:
# `git commit -m "… streamlib run …"`, `sed 's|streamlib run|…|' README.md`.
ENV_ASSIGN="([A-Za-z_][A-Za-z0-9_]*=(\"[^\"]*\"|'[^']*'|[^[:space:]]*)[[:space:]]+)*"
TEXT_TOOL_LINE="^[[:space:]]*${ENV_ASSIGN}(git|gh|sed|grep|rg|awk|perl|python3?|node|echo|printf|cat|jq|diff|rev|tee)([[:space:]]|$)"
candidates="$(printf '%s\n' "$stripped" | grep -Ev -- "$TEXT_TOOL_LINE")"

# Every key is anchored to a command position: the start of a line or just past a
# separator, after env assignments and exec wrappers. `bash -c "…"` bodies stay
# unparsed strings, so a launch inside one is not seen.
WRAPPER="(nohup|timeout|env|stdbuf|xvfb-run|uv|poetry)"
COMMAND_POSITION="(^|[;&|(])[[:space:]]*${ENV_ASSIGN}(${WRAPPER}([[:space:]]+[^[:space:]]+)*[[:space:]]+)*"
STREAMLIB_LAUNCH_KEY="${COMMAND_POSITION}([[:alnum:]_./-]*/)?streamlib[[:space:]]+(run|dev)([[:space:]]|$)"
CARGO_RUN_KEY="${COMMAND_POSITION}cargo[[:space:]]+run([[:space:]]|$)"
E2E_WRAPPER="(nohup|timeout|env|stdbuf|bash|sh|source|\\.)"
E2E_SCRIPT_KEY="(^|[;&|(])[[:space:]]*${ENV_ASSIGN}(${E2E_WRAPPER}([[:space:]]+[^[:space:]]+)*[[:space:]]+)*([[:alnum:]_./-]*/)?tests/fixtures/e2e_[[:alnum:]_./-]*\\.sh([[:space:]]|$)"

line_hit() { printf '%s\n' "$candidates" | grep -Eq -- "$1"; }
cwd_has() { printf '%s' "$cwd" | grep -Eq -- "$1"; }
names_an_example() {
  cwd_has '(^|/)examples(/|$)' || printf '%s\n' "$stripped" | grep -Eq -- '(^|[^[:alnum:]_.-])examples/'
}

matched_rules=""
matched_prefix=""
note_rule() { matched_rules="${matched_rules:+$matched_rules }$1"; }
# The first matching line, cut after the key, seeds the glob the owner is offered.
remember_match() {
  [ -z "$matched_prefix" ] || return 0
  matched_prefix="$(printf '%s\n' "$1" | grep -E -m1 -- "$2" | grep -Eo -- "^.*($2)" | head -1)"
}

if line_hit '\bffmpeg\b' && line_hit '(^|[[:space:]])-f[[:space:]]+v4l2|/dev/video[0-9]+'; then
  note_rule ffmpeg_v4l2
  remember_match "$candidates" 'ffmpeg'
fi

# `--help` prints and exits, and examples/* are not workspace members so a `-p`
# spelling of `cargo run` never reaches one.
launch_lines="$(printf '%s\n' "$candidates" \
  | grep -Ev -- 'streamlib[[:space:]]+(run|dev)([[:space:]]+[^[:space:]]+)*[[:space:]]+(--help|-h)([[:space:]]|$)' \
  | grep -Ev -- 'cargo[[:space:]]+run([[:space:]]+[^[:space:]]+)*[[:space:]]+((-p|--package)([[:space:]]|=)|(--help|-h)([[:space:]]|$))')"
if names_an_example \
   && printf '%s\n' "$launch_lines" | grep -Eq -- "${STREAMLIB_LAUNCH_KEY}|${CARGO_RUN_KEY}"; then
  note_rule example_launch
  remember_match "$launch_lines" 'streamlib[[:space:]]+(run|dev)|cargo[[:space:]]+run'
fi

e2e_lines="$(printf '%s\n' "$candidates" \
  | grep -Ev -- '(^|[[:space:]])(bash|sh)[[:space:]]+-[[:alpha:]]*n[[:alpha:]]*([[:space:]]|$)')"
if printf '%s\n' "$e2e_lines" | grep -Eq -- "$E2E_SCRIPT_KEY"; then
  note_rule e2e_fixture
  remember_match "$e2e_lines" 'e2e_[[:alnum:]_.-]*\.sh'
fi

if line_hit '\b(ffplay|mpv|gst-launch(-[0-9.]+)?)\b' && line_hit '/dev/video[0-9]+'; then
  note_rule player_on_device
  remember_match "$candidates" 'ffplay|mpv|gst-launch(-[0-9.]+)?'
fi

# Query verbs (--list-*, --get-*, --all, --info, -D) are not streaming.
if line_hit '\bv4l2-ctl\b' \
   && line_hit '--stream-(mmap|user|to|out|dmabuf|from|dqmax)|--stream[[:space:]=]'; then
  note_rule v4l2ctl_stream
  remember_match "$candidates" 'v4l2-ctl'
fi

# ── the outcome ──────────────────────────────────────────────────────
glob_hit() {
  local pattern
  while IFS= read -r pattern; do
    [ -n "$pattern" ] || continue
    # shellcheck disable=SC2053
    if [[ "$cmd" == $pattern ]]; then
      printf '%s' "$pattern"
      return 0
    fi
  done < <(jq -r --arg list "$1" '.[$list][]' <<<"$config")
  return 1
}
rule_outcome() { jq -r --arg rule "$1" '.rules[$rule] // .mode' <<<"$config"; }
rule_outcome_source() {
  if [ -n "$(jq -r --arg rule "$1" '.rules[$rule] // ""' <<<"$config")" ]; then
    printf 'rules.%s' "$1"
  else
    printf 'mode'
  fi
}

outcome=""
ask_source=""
stop_asking=""
if ask_glob="$(glob_hit ask)"; then
  outcome=ask
  ask_source="the ask glob '${ask_glob}'"
  stop_asking="${HELPER} remove '${ask_glob}'"
  [ -n "$matched_rules" ] || note_rule ask_glob
elif glob_hit allow >/dev/null; then
  exit 0
elif [ -z "$matched_rules" ]; then
  exit 0
else
  outcome=off
  for rule in $matched_rules; do
    case "$(rule_outcome "$rule")" in
      ask)
        outcome=ask
        ask_source="$(rule_outcome_source "$rule")"
        stop_asking="${HELPER} rule ${rule} warn"
        ;;
      warn) [ "$outcome" = ask ] || outcome=warn ;;
    esac
  done
  [ "$outcome" != off ] || exit 0
fi

# ── the messages ─────────────────────────────────────────────────────
describe_rule() {
  case "$1" in
    ffmpeg_v4l2) printf 'ffmpeg reading a camera device or writing a v4l2 output' ;;
    example_launch) printf 'launching an app under examples/, which opens a camera or a display window' ;;
    e2e_fixture) printf 'executing an e2e_ fixture script under tests/fixtures/, which drives the rig' ;;
    player_on_device) printf 'a media player pointed at a camera device node' ;;
    v4l2ctl_stream) printf 'v4l2-ctl streaming from a device' ;;
    ask_glob) printf 'a command matching an ask glob in the rig-brake config' ;;
  esac
}
rules_text=""
first_rule=""
for rule in $matched_rules; do
  [ -n "$first_rule" ] || first_rule="$rule"
  rules_text="${rules_text:+$rules_text, }${rule} ($(describe_rule "$rule"))"
done
[ -n "$matched_prefix" ] || matched_prefix="$(printf '%s\n' "$candidates" | grep -m1 . | cut -c1-60)"
suggested_glob="*$(printf '%s' "$matched_prefix" \
  | sed -E 's/^[[:space:]]+//; s/([][*?\\])/\\\1/g; s/'"'"'/?/g')*"
preferences="$(jq -r '.preferences' <<<"$config")"

if [ "$outcome" = ask ]; then
  reason="${config_problems}rig-brake: ${rules_text}. Set to ask by ${ask_source}. Approve to run it with the sandbox bypass; decline to park it for /verify-live, where an unattended firing would otherwise die at exit 144. To stop asking: ${stop_asking}"
  jq -n --arg reason "$reason" \
    '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "ask", permissionDecisionReason: $reason}}'
  exit 0
fi

system_message="${config_problems}rig-brake: ${first_rule} noted, not a prompt. Silence it: ${HELPER} rule ${first_rule} off  or  ${HELPER} allow '${suggested_glob}'"
context="rig-brake note, advisory only, nothing was blocked: this command matched rule ${rules_text}. It drives the rig. That is fine when the owner asked for this eval. In an unattended or sandboxed firing it dies at exit 144, so park it for /verify-live instead. Control-plane reads (streamlib nodes, graph, tap, logs, exchange) never trigger this note. Owner preferences: ${preferences:-none recorded}. The owner can silence this for good with \`${HELPER} rule ${first_rule} off\` or \`${HELPER} allow '${suggested_glob}'\`; offer that if they say the command is fine, and never add an exception yourself."
jq -n --arg message "$system_message" --arg context "$context" \
  '{systemMessage: $message, hookSpecificOutput: {hookEventName: "PreToolUse", additionalContext: $context}}'
exit 0
