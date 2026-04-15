# PROMPT.md — Task Execution Protocol

When told to "review PROMPT.md and execute the next task", follow this protocol exactly.

---

## Step 1: Find the Next Task

Run `amos graph` to see all tasks and their statuses. Find the first task with
`status: pending` whose dependencies are all `completed` or `CLOSED`. That is
the next task.

Read the task's plan file (e.g., `plan/249-moq-subgroup-fix.md`) with `amos show {name}`
to get the full task description.

---

## Step 2: Announce the Task

Before doing any work, output this to the user:

```
## Starting Task

- **Issue**: #{number} — {title}
- **Branch**: `{branch-name}`
- **Summary**: {1-2 sentence description of what will be done}
- **Files in scope**: {list of files/directories that will be touched}
- **Estimated scope**: {small / medium / large}
```

Wait for the user to confirm before proceeding. If they say go, proceed.

---

## Step 3: Create the Branch

```bash
git checkout main && git pull origin main
git checkout -b {branch-name}
```

The branch name is specified in the amos task description.

---

## Step 4: Do the Work

- Follow the task description in the amos plan file
- Stay focused on the scope described in the issue
- If you discover something that needs fixing but is outside this issue's
  scope, note it as a follow-up — do NOT fix it in this branch
- Run `cargo check` frequently to catch errors early
- Make small, logical commits with conventional commit messages

---

## Step 5: Test

Run appropriate tests and output results in this format:

```
## Test Results

- **cargo check**: ✅ pass / ❌ fail
- **cargo test**: ✅ {n} passed, {n} failed / ⚠️ skipped (reason)
- **cargo build -p {relevant-package}**: ✅ pass / ❌ fail
- **Integration**: {description of any manual verification done}

### Issues Found
- {any issues, or "None"}
```

If tests fail, fix them before proceeding. If a failure is out of scope,
document it and move on.

---

## Step 6: Push and Open PR

```bash
git push -u origin {branch-name}
gh pr create --title "{conventional-commit-style title}" \
  --body "$(cat <<'EOF'
## Summary
{1-3 bullet points of what changed}

## Issue
Closes #{number}

## Test Plan
- [ ] `cargo check` passes
- [ ] `cargo test` passes
- [ ] {any specific verification steps}

## Follow-ups
- {anything discovered that's out of scope, or "None"}
EOF
)"
```

---

## Step 7: Report Completion

Output this to the user:

```
## Task Complete

- **Issue**: #{number} — {title}
- **Branch**: `{branch-name}`
- **PR**: {link to PR}
- **Commits**: {number of commits}
- **Files changed**: {number}
- **Lines**: +{added} / -{removed}

### What was done
{brief description of changes}

### What was NOT done (follow-ups)
{anything out of scope that was noted, or "None"}

### Ready for review
The PR is open and ready for your review. Do NOT merge — I will not
merge PRs without explicit instruction.
```

---

## Step 8: Update the Amos Task

Update the task's plan file to set `status: in_review` (or `completed` if
the user has already approved and merged).

---

## Rules

1. **One branch per task.** Never put work from multiple tasks on one branch.
2. **Never merge to main.** Only create PRs.
3. **Never modify files outside scope.** Note follow-ups instead.
4. **Always announce before starting.** Wait for user confirmation.
5. **Always test before pushing.** Don't push broken code.
6. **Use amos for task state.** Update plan files when status changes.
7. **Conventional commits.** `fix:`, `feat:`, `refactor:`, `docs:`, etc.
