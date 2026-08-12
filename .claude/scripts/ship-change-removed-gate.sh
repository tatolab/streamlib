#!/usr/bin/env bash
# ship-change gate: every `- REMOVED: <pattern>` bullet in a change file must name
# something that no longer exists in the tree. Run from the repo root.
#
# Usage: ship-change-removed-gate.sh docs/plan/changes/<name>.md
# Exit 0 = clean; 1 = residue still present, or a bullet that cannot prove anything;
# 2 = usage / not a repo.
#
# A bullet is checked two independent ways, because neither alone is a removal proof:
#
#   contents  git grep -F over tracked files — "nothing references this any more".
#   path      git ls-files over the same pattern read as a literal path — "the file or
#             directory is gone". git grep never matches a filename, so an unreferenced
#             leaf file is invisible to the content sweep; a path-shaped bullet without
#             this check certifies only that nothing mentions it.
#
# Bullet grammar, enforced below — one pattern per bullet, first physical line, plain
# text. A pattern that carries markdown decoration or joins several items can never
# match a definition site, so it would pass green forever; those are rejected rather
# than searched. Continuation lines are prose and are not searched.
set -euo pipefail

change="${1:?usage: ship-change-removed-gate.sh <change-file>}"
[ -f "$change" ] || { echo "no such change file: $change" >&2; exit 2; }
git rev-parse --git-dir >/dev/null 2>&1 || { echo "must run inside the repo — a gate that cannot search must not pass" >&2; exit 2; }

fail=0
hits="$(mktemp)"
trap 'rm -f "$hits"' EXIT

# Excluded from the content sweep only — each is a surface that names a removed
# artifact by policy or by design, so a hit there is not residue the engine can act on:
#   CHANGELOG.md      release-please generates it from merged commit subjects; the
#                     entry announcing a removal is unscrubbable.
#   docs/decisions/** annotate-don't-overwrite (.claude/rules/docs-policy.md): a
#                     superseded ADR keeps naming what it retired.
#   docs/learnings/** empirical-only (same rule): a learning records driver or library
#                     behaviour that stays true after the thing that surfaced it is gone.
#   docs/plan/**      the plan states what we agreed, not what the tree holds. It also
#                     breaks a deadlock: /ship-change gates at step 1 but folds
#                     ARCHITECTURE.md at step 3, so a change whose own plan text names
#                     what it removes could never reach the step that retires that text.
#   examples/**       consumers, lagging by design (CLAUDE.md).
#   packages/<consumer>  the downstream-consumer entries only, same doctrine — see below.
#   vendor/**         the vendored vulkanalia fork, never ours to edit.
# The path check inherits none of these: a file existing at a named path is residue
# wherever it lives, so a bullet naming e.g. a packages/ leaf or a
# docs/plan file still fails while that path is tracked.
content_excludes=(
  ':!vendor/**'
  ':!docs/plan/**'
  ':!docs/decisions/**'
  ':!docs/learnings/**'
  ':!examples/**'
  ':!CHANGELOG.md'
)

# packages/ is split, and the split is load-bearing (CLAUDE.md): these entries are
# engine-side — not downstream consumers — so they stay in the sweep and their residue is
# real work. Everything else under packages/ is a consumer that lags by design and is
# excluded. Derived from the tree rather than hard-listed, so a package added later is
# excluded by default and this list stays the only thing to maintain.
engine_side_packages=" escalate core test-fixtures "
for pkg_dir in packages/*/; do
  [ -d "$pkg_dir" ] || continue
  pkg_name="${pkg_dir#packages/}"
  pkg_name="${pkg_name%/}"
  case "$engine_side_packages" in *" $pkg_name "*) continue ;; esac
  content_excludes+=(":!$pkg_dir**")
done

reject() {
  echo "MALFORMED BULLET: ${1:-(empty)}"
  echo "  $2"
  fail=1
}

count=0
while IFS= read -r pat; do
  count=$((count + 1))

  # An empty pattern is a declared removal that searches for nothing. Counted and
  # rejected, never skipped: skipping it would also under-report the bullet total, so
  # the summary line would vouch for a file it had not fully read.
  if [ -z "$pat" ]; then
    reject "$pat" "no pattern after 'REMOVED:' — write the artifact on the bullet's own line."
    continue
  fi

  case "$pat" in
    *'`'*)
      reject "$pat" "backticked — backticks appear only in markdown and rustdoc prose, never in a definition. Write the pattern as plain text."
      continue ;;
    *' / '*)
      reject "$pat" "joins several items — the whole line is searched as one literal. Split it into one bullet per item."
      continue ;;
    *'{'*)
      reject "$pat" "shell brace expansion is not expanded — it is searched literally and can never hit. Write one bullet per expanded name."
      continue ;;
    *' ('*)
      reject "$pat" "trailing parenthetical is part of the searched literal. Move the note to a continuation line."
      continue ;;
    .|./|/|..)
      reject "$pat" "degenerate pattern — it would match the whole tree."
      continue ;;
  esac

  # Tracked files only: build scratch and caches must not be able to fake a pass or a
  # failure. Judge by captured matches, not by the exit code.
  git grep -InF -- "$pat" -- "${content_excludes[@]}" >"$hits" 2>/dev/null || true
  if [ -s "$hits" ]; then
    echo "STILL PRESENT (referenced): $pat"
    head -20 "$hits" | sed 's/^/  /'
    fail=1
  fi

  # `:(literal)` so a pattern containing glob metacharacters is read as the path it
  # names, not as a wildcard. A symbol-shaped pattern names no path and matches nothing.
  # No exclusions here, by design — a file sitting at the named path is residue wherever
  # it lives, vendor/ included. Never add a `:!` pathspec to this call either: git (2.43)
  # silently drops an exact-file positive match the moment any exclude joins the list,
  # which would make every leaf-file bullet — the one case this check exists for — pass
  # green.
  git ls-files -- ":(literal)$pat" >"$hits" 2>/dev/null || true
  if [ -s "$hits" ]; then
    echo "STILL PRESENT (on disk): $pat"
    head -20 "$hits" | sed 's/^/  /'
    fail=1
  fi
done < <(sed -n 's/^[-*][[:space:]]*REMOVED:[[:space:]]*//p' "$change")

if [ "$count" -eq 0 ]; then
  echo "note: $change declares no '- REMOVED:' bullets — nothing to verify."
elif [ "$fail" -eq 0 ]; then
  echo "clean: $count REMOVED bullets, none referenced and none on disk."
fi

exit "$fail"
