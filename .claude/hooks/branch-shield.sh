#!/usr/bin/env bash
# branch-shield (PreToolUse / Bash): refuse irreversible git/gh actions against main.
# Exit 2 + stderr = deny; exit 0 = allow. Quoted substrings are stripped before
# matching so a commit message that mentions a guarded phrase can't false-trigger.

input="$(cat)"
cmd="$(printf '%s' "$input" | jq -r '.tool_input.command // ""')"
cwd="$(printf '%s' "$input" | jq -r '.cwd // ""')"

stripped="$(printf '%s' "$cmd" | sed -E "s/'[^']*'//g; s/\"[^\"]*\"//g")"

deny() {
  echo "branch-shield: refused — $1" >&2
  exit 2
}

match() { printf '%s' "$stripped" | grep -Eq "$1"; }

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

# git commit while the current branch is main or master.
if match 'git[[:space:]]+commit'; then
  branch="$(git -C "${cwd:-.}" branch --show-current 2>/dev/null || true)"
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
