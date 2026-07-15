#!/usr/bin/env bash
# branch-shield (PreToolUse / Bash): refuse irreversible git/gh actions against main.
# Exit 2 + stderr = deny; exit 0 = allow.
#
# Quote handling is two-stage: first UNWRAP quotes around a single bare token
# (`"origin"` -> origin) so `git push "origin" main` still matches; then STRIP
# any remaining quoted runs (multi-word commit-message payloads) so a message
# mentioning a guarded phrase can't false-trigger.

input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // ""')"
cwd="$(printf '%s' "$input" | jq -r '.cwd // ""')"

stripped="$(printf '%s' "$cmd" | sed -E \
  -e 's/"([[:alnum:]_./:@=+-]+)"/\1/g' \
  -e "s/'([[:alnum:]_./:@=+-]+)'/\1/g" \
  -e "s/'[^']*'//g" \
  -e 's/"[^"]*"//g')"

# Resolve the git target directory: `git -C <path>` wins over the payload cwd for
# the on-main check, and is normalized out of the command so subcommand matchers
# see `git commit` / `git push` directly.
gitc_path="$(printf '%s' "$stripped" \
  | grep -oE 'git[[:space:]]+-C[[:space:]]+[^[:space:]]+' \
  | head -n1 | sed -E 's/^git[[:space:]]+-C[[:space:]]+//')"
target_dir="${gitc_path:-${cwd:-.}}"
norm="$(printf '%s' "$stripped" | sed -E 's/git[[:space:]]+-C[[:space:]]+[^[:space:]]+[[:space:]]+/git /g')"

deny() {
  echo "branch-shield: refused — $1" >&2
  exit 2
}

match() { printf '%s' "$norm" | grep -Eq "$1"; }

# gh pr merge — merging is Jonathan's call.
if match 'gh[[:space:]]+pr[[:space:]]+merge'; then
  deny "merging PRs is Jonathan's call (gh pr merge)"
fi

# git push targeting origin main (force or not): 'origin main', 'origin +main',
# 'origin HEAD:main', or any '<refspec>:main'. Boundary excludes 'main-feature'.
if match 'git[[:space:]]+push' \
   && match '(origin[[:space:]]+\+?(HEAD:)?main|:main)([^[:alnum:]._/-]|$)'; then
  deny "pushing to origin main is never allowed"
fi

# git commit while the target repo's current branch is main or master.
if match 'git[[:space:]]+commit'; then
  branch="$(git -C "$target_dir" branch --show-current 2>/dev/null || true)"
  if [ "$branch" = "main" ] || [ "$branch" = "master" ]; then
    deny "committing directly on '$branch' — branch first"
  fi
fi

# git branch -D main
if match 'git[[:space:]]+branch[[:space:]]+-D[[:space:]]+main([^[:alnum:]._/-]|$)'; then
  deny "deleting the main branch is never allowed"
fi

# git reset --hard origin/<ref>
if match 'git[[:space:]]+reset[[:space:]]+--hard[[:space:]]+origin/'; then
  deny "hard-resetting to a remote ref discards local work"
fi

exit 0
