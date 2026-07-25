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

# This hook ESCALATES; it never decides. Every rule below matches something the
# agent must not do on its own initiative — but each is also something the owner
# may legitimately want, and only they can tell which case this is. A hard deny
# (exit 2) takes the final say, which leaves an owner driving remotely with no way
# to approve a legitimate exception. So every rule asks.
#
# This is not a weakening. `permissionDecision: "ask"` surfaces the normal
# permission prompt on EVERY match, and an unattended firing has nobody to answer
# it — so autonomous runs are still stopped cold. The escalation only opens a door
# the owner is already standing at. Same contract as rig-brake.sh.
#
# The corollary is that these prompts must stay RARE. A rule that fires in normal
# operation trains the owner to approve reflexively, which is worse than no rule
# at all — so each pattern below is narrow on purpose, and a false positive is a
# bug to fix rather than noise to live with.
ask() {
  jq -n --arg r "$1" \
    '{hookSpecificOutput: {hookEventName: "PreToolUse", permissionDecision: "ask", permissionDecisionReason: $r}}'
  exit 0
}

match() { printf '%s' "$norm" | grep -Eq "$1"; }

# gh pr merge. Never file-gated: the old gitignored authorization toggle was
# unreachable from remote control and, being gitignored, absent from every
# worktree — so a merge the owner had granted was silently denied from all of them.
if match 'gh[[:space:]]+pr[[:space:]]+merge'; then
  ask "Merging a PR is always the owner's call. Approve only if you intend this merge to land now; decline to leave the PR open for review."
fi

# git push targeting origin main (force or not): 'origin main', 'origin +main',
# 'origin HEAD:main', or any '<refspec>:main'. Boundary excludes 'main-feature'.
if match 'git[[:space:]]+push' \
   && match '(origin[[:space:]]+\+?(HEAD:)?main|:main)([^[:alnum:]._/-]|$)'; then
  ask "This pushes directly to origin/main, bypassing the PR flow the repo squash-merges through. The agent has no legitimate reason to do this — approve only if you are deliberately pushing to main yourself."
fi

# git commit while the target repo's current branch is main or master.
if match 'git[[:space:]]+commit'; then
  branch="$(git -C "$target_dir" branch --show-current 2>/dev/null || true)"
  if [ "$branch" = "main" ] || [ "$branch" = "master" ]; then
    ask "This commits directly on '$branch' rather than a branch. Note that the loop's runtime state under .claude/loops/state/ is gitignored precisely because this is refused — approving a commit that tracks loop state would undo that. Branch first unless you mean this."
  fi
fi

# git branch -D main
if match 'git[[:space:]]+branch[[:space:]]+-D[[:space:]]+main([^[:alnum:]._/-]|$)'; then
  ask "This force-deletes the local main branch. Approve only if you are deliberately recreating it."
fi

# git reset --hard origin/<ref>
if match 'git[[:space:]]+reset[[:space:]]+--hard[[:space:]]+origin/'; then
  ask "This hard-resets to a remote ref and DISCARDS any local work in this tree, including uncommitted changes. Approve only if you know this tree has nothing you need."
fi

exit 0
