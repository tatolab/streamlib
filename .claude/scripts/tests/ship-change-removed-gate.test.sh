#!/usr/bin/env bash
# Unit tests for .claude/scripts/ship-change-removed-gate.sh.
#
# Each case builds a throwaway git repo, plants an artifact, and asserts what the gate
# says about it. The gate is the only mechanism that proves a ripout landed, so every
# way a bullet can pass vacuously gets a case here — a green gate on a tree that still
# holds the artifact is the failure mode this file exists to catch.
#
# No toolchain, no network: bash + git.
set -uo pipefail

gate="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/ship-change-removed-gate.sh"
[ -f "$gate" ] || { echo "gate script not found at $gate" >&2; exit 2; }

passed=0
failed=0
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

case_no=0
out=""
status=0

# new_repo — a fresh repo in $repo with the change file at docs/plan/changes/c.md.
# Bullets are passed as one heredoc-ish string on stdin.
new_repo() {
  case_no=$((case_no + 1))
  repo="$work/case-$case_no"
  mkdir -p "$repo/docs/plan/changes"
  git -C "$repo" init -q
  git -C "$repo" config user.email t@example.com
  git -C "$repo" config user.name t
  { printf '# Change: c\n\n## REMOVED\n\n'; cat; } >"$repo/docs/plan/changes/c.md"
}

# plant <path> <contents> — a tracked file. Staged, not committed: git grep and
# git ls-files both read the index, which is what the gate searches.
plant() {
  mkdir -p "$repo/$(dirname "$1")"
  printf '%s\n' "$2" >"$repo/$1"
  git -C "$repo" add -- "$1"
}

run_gate() {
  git -C "$repo" add -- docs/plan/changes/c.md
  out="$(cd "$repo" && bash "$gate" docs/plan/changes/c.md 2>&1)"
  status=$?
}

ok() { passed=$((passed + 1)); printf '  ok   %s\n' "$1"; }
bad() {
  failed=$((failed + 1))
  printf '  FAIL %s\n' "$1"
  printf '%s\n' "$out" | sed 's/^/       | /'
}

expect_pass() { if [ "$status" -eq 0 ]; then ok "$1"; else bad "$1 (expected exit 0, got $status)"; fi; }
expect_fail() {
  if [ "$status" -eq 1 ] && printf '%s' "$out" | grep -qF -- "$2"; then
    ok "$1"
  else
    bad "$1 (expected exit 1 mentioning '$2', got $status)"
  fi
}

echo "ship-change-removed-gate.sh"

# --- the baseline: absence passes, presence fails -------------------------------

new_repo <<'EOF'
- REMOVED: ProcessorVTable
EOF
plant src/lib.rs "pub struct Something;"
run_gate
expect_pass "a symbol that is genuinely gone passes"

new_repo <<'EOF'
- REMOVED: ProcessorVTable
EOF
plant src/lib.rs "pub struct ProcessorVTable { slot: usize }"
run_gate
expect_fail "a symbol still defined in a tracked file fails" "STILL PRESENT (referenced): ProcessorVTable"

new_repo <<'EOF'
* REMOVED: ProcessorVTable
EOF
plant src/lib.rs "pub struct ProcessorVTable;"
run_gate
expect_fail "a '*' bullet marker is parsed too" "STILL PRESENT (referenced): ProcessorVTable"

new_repo <<'EOF'
- REMOVED: ProcessorVTable
EOF
mkdir -p "$repo/src"
printf 'pub struct ProcessorVTable;\n' >"$repo/src/untracked.rs"
run_gate
expect_pass "an untracked file cannot fake a failure"

# --- defect 6: git grep never matches a filename ---------------------------------

new_repo <<'EOF'
- REMOVED: tools/streamlib-pack
EOF
plant tools/streamlib-pack/src/main.rs "fn main() {}"
run_gate
expect_fail "a directory that still exists fails even when nothing references it" \
  "STILL PRESENT (on disk): tools/streamlib-pack"

new_repo <<'EOF'
- REMOVED: schemas/streamlib.schema.json
EOF
plant schemas/streamlib.schema.json '{"title":"unreferenced leaf"}'
run_gate
expect_fail "an unreferenced leaf file fails on the path check" \
  "STILL PRESENT (on disk): schemas/streamlib.schema.json"

new_repo <<'EOF'
- REMOVED: tools/streamlib-pack
EOF
plant tools/streamlib-keep/src/main.rs "fn main() {}"
run_gate
expect_pass "a sibling directory is not mistaken for the removed one"

new_repo <<'EOF'
- REMOVED: sdk/a[b].rs
EOF
plant sdk/ab.rs "fn a() {}"
run_gate
expect_pass "a glob metacharacter in a bullet is read as a literal path, not a wildcard"

# --- defect 7: surfaces that name a removed artifact by policy -------------------

new_repo <<'EOF'
- REMOVED: streamlib-pack
EOF
plant CHANGELOG.md "* remove streamlib-pack (#1715)"
run_gate
expect_pass "a release-please CHANGELOG entry is not residue"

new_repo <<'EOF'
- REMOVED: streamlib-pack
EOF
plant docs/decisions/packaging.md "> ~~streamlib-pack builds the artifact.~~ — Superseded."
run_gate
expect_pass "a superseded ADR that still names the artifact is not residue"

new_repo <<'EOF'
- REMOVED: streamlib-pack
EOF
plant vendor/tatolab-vulkanalia/src/lib.rs "// streamlib-pack"
run_gate
expect_pass "the vendored tree is out of scope for the content sweep"

# ...but only the content sweep. The path check has no exclusions at all: a file at the
# named path is residue wherever it lives, and a bullet that retires the vendored fork
# must not pass while the fork is still checked in.
new_repo <<'EOF'
- REMOVED: vendor/tatolab-vulkanalia
EOF
plant vendor/tatolab-vulkanalia/src/lib.rs "pub fn vk_create() {}"
run_gate
expect_fail "a vendored path that still exists is residue" \
  "STILL PRESENT (on disk): vendor/tatolab-vulkanalia"

new_repo <<'EOF'
- REMOVED: streamlib-pack
EOF
plant docs/plan/changes/archive/2026-01-01-other.md "- REMOVED: streamlib-pack"
run_gate
expect_pass "another change file's own inventory is not residue"

# The plan states what we agreed, not what the tree holds. This also breaks a deadlock:
# /ship-change gates at step 1 but folds ARCHITECTURE.md at step 3, so a change whose own
# plan text names what it removes could never reach the step that retires that text.
new_repo <<'EOF'
- REMOVED: streamlib_modules
EOF
plant docs/plan/ARCHITECTURE.md "deleted in full: \`streamlib_modules/\`, the .slpkg format, streamlib.lock"
run_gate
expect_pass "the plan's own description of a removal is not residue"

# Empirical records outlive the thing that surfaced them.
new_repo <<'EOF'
- REMOVED: .slpkg
EOF
plant docs/learnings/slpkg-raw-device-rhi-construction.md "A separately-built .slpkg double-frees in vkCreatePipelineLayout."
run_gate
expect_pass "a learning that records driver behaviour is not residue"

# Consumers lag by design (CLAUDE.md) and are moving out-of-repo (#1672).
new_repo <<'EOF'
- REMOVED: streamlib_modules
EOF
plant examples/camera-display/src/main.rs "let dir = home.join(\"streamlib_modules\");"
run_gate
expect_pass "an example that still uses the artifact is not residue"

new_repo <<'EOF'
- REMOVED: .slpkg
EOF
plant packages/h264/src/lib.rs "// resolves a .slpkg from the store"
run_gate
expect_pass "a consumer package is not residue"

# packages/ is split and the split is load-bearing (CLAUDE.md): these three are engine-side,
# not downstream consumers, so their residue is real work and must still be reported.
for engine_pkg in escalate core test-fixtures; do
  new_repo <<'EOF'
- REMOVED: .slpkg
EOF
  plant "packages/$engine_pkg/src/lib.rs" "// resolves a .slpkg from the store"
  run_gate
  expect_fail "packages/$engine_pkg is engine-side and stays in the sweep" \
    "STILL PRESENT (referenced): .slpkg"
done

# ...but the path check inherits none of those exclusions. A bullet naming a path inside
# an excluded tree still fails while that path is tracked — otherwise this change's own
# `packages/test-fixtures-abi-mismatch` bullet would pass green forever.
new_repo <<'EOF'
- REMOVED: packages/test-fixtures-abi-mismatch
EOF
plant packages/test-fixtures-abi-mismatch/Cargo.toml "[package]"
run_gate
expect_fail "a path inside a content-excluded tree is still checked for existence" \
  "STILL PRESENT (on disk): packages/test-fixtures-abi-mismatch"

new_repo <<'EOF'
- REMOVED: docs/plan/changes/dead-change.md
EOF
plant docs/plan/changes/dead-change.md "# Change: dead"
run_gate
expect_fail "a plan path that still exists is residue" \
  "STILL PRESENT (on disk): docs/plan/changes/dead-change.md"

# A file that lives at the excluded path is still residue when the bullet names the
# path itself — the content exclusions must not leak into the path check.
new_repo <<'EOF'
- REMOVED: docs/decisions/packaging.md
EOF
plant docs/decisions/packaging.md "unrelated prose"
run_gate
expect_fail "an excluded-from-grep path is still checked for existence" \
  "STILL PRESENT (on disk): docs/decisions/packaging.md"

# --- defects 1-4: bullets that can never match -----------------------------------

new_repo <<'EOF'
- REMOVED: `tools/streamlib-pack`
EOF
plant tools/streamlib-pack/src/main.rs "fn main() {}"
run_gate
expect_fail "a backticked bullet is rejected, not searched" "MALFORMED BULLET"

new_repo <<'EOF'
- REMOVED: `tools/streamlib-pack`
EOF
run_gate
expect_fail "a backticked bullet is rejected even when the tree is clean" "MALFORMED BULLET"

new_repo <<'EOF'
- REMOVED: streamlib-adapter-vulkan-abi / streamlib-adapter-vulkan-helpers
EOF
run_gate
expect_fail "a '/'-joined bullet is rejected" "joins several items"

new_repo <<'EOF'
- REMOVED: tools/streamlib-cli/src/commands/{add,install}.rs
EOF
run_gate
expect_fail "a brace-expansion bullet is rejected" "brace expansion"

new_repo <<'EOF'
- REMOVED: native_lib_resolver (the cdylib resolution)
EOF
run_gate
expect_fail "a trailing parenthetical is rejected" "trailing parenthetical"

new_repo <<'EOF'
- REMOVED: .
EOF
run_gate
expect_fail "a degenerate whole-tree pattern is rejected" "degenerate pattern"

new_repo <<'EOF'
- REMOVED:
EOF
run_gate
expect_fail "a bullet with no pattern is rejected, not skipped" "no pattern after 'REMOVED:'"

# An empty bullet must not vanish from the count either — a summary that says "2 bullets"
# for a file declaring 3 vouches for a bullet it never read.
new_repo <<'EOF'
- REMOVED: Alpha
- REMOVED:
- REMOVED: Beta
EOF
run_gate
if [ "$status" -eq 1 ] && printf '%s' "$out" | grep -qF "MALFORMED BULLET: (empty)"; then
  ok "an empty bullet among good ones fails the file"
else
  bad "an empty bullet among good ones fails the file (got $status)"
fi

# The rejections must not fire on legitimate patterns.
new_repo <<'EOF'
- REMOVED: host_callbacks()
- REMOVED: runtime/streamlib-engine/src/core/plugin/
- REMOVED: export_plugin!
- REMOVED: RunnerAutoBuild
EOF
run_gate
expect_pass "a call-shaped, path-shaped, macro-shaped or plain symbol is not rejected"

# --- defect 5: only the first physical line is a pattern -------------------------

new_repo <<'EOF'
- REMOVED: ProcessorVTable
  The vtable and its layout regression test, both retired with the plugin ABI.
EOF
plant src/lib.rs "// the layout regression test lived here"
run_gate
expect_pass "a continuation line is prose, not a second pattern"

# --- shape and usage -------------------------------------------------------------

new_repo <<'EOF'
- REMOVED: Alpha
- REMOVED: Beta
EOF
run_gate
if [ "$status" -eq 0 ] && printf '%s' "$out" | grep -qF "clean: 2 REMOVED bullets"; then
  ok "a clean run reports how many bullets it actually checked"
else
  bad "a clean run reports how many bullets it actually checked (got $status)"
fi

new_repo <<'EOF'
EOF
run_gate
if [ "$status" -eq 0 ] && printf '%s' "$out" | grep -qF "declares no '- REMOVED:' bullets"; then
  ok "a change file with no REMOVED bullets says so"
else
  bad "a change file with no REMOVED bullets says so (got $status)"
fi

new_repo <<'EOF'
- REMOVED: Alpha
EOF
out="$(cd "$repo" && bash "$gate" docs/plan/changes/nope.md 2>&1)"
status=$?
if [ "$status" -eq 2 ]; then ok "a missing change file exits 2"; else bad "a missing change file exits 2 (got $status)"; fi

new_repo <<'EOF'
- REMOVED: Alpha
EOF
outside="$work/not-a-repo"
mkdir -p "$outside/docs/plan/changes"
cp "$repo/docs/plan/changes/c.md" "$outside/docs/plan/changes/c.md"
out="$(cd "$outside" && bash "$gate" docs/plan/changes/c.md 2>&1)"
status=$?
if [ "$status" -eq 2 ]; then ok "outside a repo the gate refuses to pass"; else bad "outside a repo the gate refuses to pass (got $status)"; fi

echo
echo "$passed passed, $failed failed"
[ "$failed" -eq 0 ]
