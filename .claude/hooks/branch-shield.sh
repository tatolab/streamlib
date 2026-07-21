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

# Resolve the git target directory for the on-main check, in priority order:
# 1. `git -C <path>` (normalized out of the command so subcommand matchers see
#    `git commit` / `git push` directly);
# 2. the last `cd <path>` preceding the git call — compound commands like
#    `cd .claude/worktrees/x && git commit` run git in the cd'd directory, not
#    the payload cwd (worktree-per-attempt makes this the canonical shape);
# 3. the payload cwd.
gitc_path="$(printf '%s' "$stripped" \
  | grep -oE 'git[[:space:]]+-C[[:space:]]+[^[:space:]]+' \
  | head -n1 | sed -E 's/^git[[:space:]]+-C[[:space:]]+//')"
cd_path="$(printf '%s' "$stripped" \
  | grep -oE '(^|&&|;)[[:space:]]*cd[[:space:]]+[^[:space:];&|]+' \
  | tail -n1 | sed -E 's/^.*cd[[:space:]]+//')"
case "$cd_path" in
  ""|/*) ;;
  *) cd_path="${cwd:-.}/$cd_path" ;;
esac
target_dir="${gitc_path:-${cd_path:-${cwd:-.}}}"
norm="$(printf '%s' "$stripped" | sed -E 's/git[[:space:]]+-C[[:space:]]+[^[:space:]]+[[:space:]]+/git /g')"

deny() {
  echo "branch-shield: refused — $1" >&2
  exit 2
}

match() { printf '%s' "$norm" | grep -Eq "$1"; }

# gh pr merge — gated on an owner-controlled authorization toggle, default OFF.
# The guardrail stays safe-by-default: the agent may merge only when the owner has
# opted in via the gitignored authorization file (single source of truth at the
# project root, read regardless of worktree). Owner toggles it from any terminal:
#   echo on  > .claude/merge-authorization.local   # grant merge to the agent
#   echo off > .claude/merge-authorization.local   # revoke (or: rm the file)
# First line must trim to exactly "on" (case-insensitive) to authorize.
if match 'gh[[:space:]]+pr[[:space:]]+merge'; then
  auth_file="${CLAUDE_PROJECT_DIR:-.}/.claude/merge-authorization.local"
  auth="$(head -n1 "$auth_file" 2>/dev/null | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]')"
  if [ "$auth" != "on" ]; then
    deny "merge authorization is OFF — owner enables with: echo on > .claude/merge-authorization.local (off/rm to revoke) (gh pr merge)"
  fi
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
