# [Project Name]: Implementation Guide for Claude Code

<approval date="YYYY-MM-DD">
**EXPLICIT APPROVAL GRANTED**: [Your name] has authorized Claude Code to work independently 
as a self-directed agent on this refactoring. All phases may be implemented without stopping 
for approval at each step.

**Branch**: `feature/your-branch-name` (can be backed out if needed)

**Constraints**: 
- [List any hard constraints]
- [e.g., "Don't modify the public API"]
- [e.g., "All tests must pass before committing"]
</approval>

<role>
You are Claude Code implementing [brief description]. This document provides the complete 
specification, current codebase state, and step-by-step implementation tasks. Follow each 
phase exactly as written, using the verification steps to confirm correctness before proceeding.
</role>

<context>
[2-3 sentences describing the project and why this refactoring is needed]
</context>

---

## Quick Reference

<status_summary>
| Feature | Status | Action Required |
|---------|--------|-----------------|
| Feature A | ✅ DONE | None |
| Feature B | 🔲 TODO | Phase 1 |
| Feature C | 🔲 TODO | Phase 2 |
</status_summary>

---

## Phase Overview

<phases>
<phase id="1" name="Phase Name" risk="low|medium|high" breaking="no|internal|public">
Brief description of what this phase accomplishes.
</phase>

<phase id="2" name="Phase Name" risk="low|medium|high" breaking="no|internal|public">
Brief description. Depends on Phase 1.
</phase>
</phases>

---

# Current Codebase State

<current_architecture>
Document the **actual current implementation** here. This prevents Claude from making 
assumptions about what exists.

<structure name="ComponentName" file="path/to/file.rs">
```rust
// Paste actual current code here
pub struct CurrentThing {
    field: Type,
}
```

**Methods**: list_of, actual, methods
**Problems**: What's wrong with this that we're fixing
</structure>

<verified status="complete|partial|none" feature="feature_name">
If something is already implemented correctly, document it here so Claude doesn't 
try to re-implement it.
</verified>
</current_architecture>

---

# Pain Points to Address

<pain_points>
<problem id="1" name="Problem Name">
<description>
Describe the problem in detail.
</description>
<solution>Brief description of the solution</solution>
</problem>

<problem id="2" name="Problem Name">
<description>
Describe the problem in detail.
</description>
<solution>Brief description of the solution</solution>
</problem>
</pain_points>

---

# Phase 1: [Phase Name]

<phase_1>
<objective>
Clear statement of what this phase accomplishes.
</objective>

<depends_on>None (or list dependencies)</depends_on>

<pre_implementation_checklist>
Before starting, verify you understand the current state:

1. [ ] Read `path/to/relevant/file.rs`
2. [ ] Identify [specific things to look for]
3. [ ] List [what needs to move/change]
</pre_implementation_checklist>

<task id="1.1" name="Task Name">
<instruction>
Clear, imperative instruction of what to do.
</instruction>

<files_to_create>
- `path/to/new/file.rs`
</files_to_create>

<files_to_modify>
- `path/to/existing/file.rs`
</files_to_modify>

<thinking>
Optional: Explain design decisions and trade-offs here.
Why this approach? What alternatives were considered?
</thinking>

<example context="Description of what this example shows">
```rust
// Concrete, working code - not pseudocode
pub fn actual_implementation() -> Result<()> {
    // Real implementation details
    Ok(())
}
```
</example>

<verification>
```bash
cargo check -p crate_name
cargo test -p crate_name test_name
```
</verification>
</task>

<task id="1.2" name="Next Task">
<!-- Repeat structure -->
</task>

<completion_checklist>
- [ ] All tests pass
- [ ] No new warnings from clippy
- [ ] Code compiles without errors
- [ ] Committed with message: `feat: [description]`
</completion_checklist>
</phase_1>

---

# Phase 2: [Phase Name]

<phase_2>
<objective>
Clear statement of what this phase accomplishes.
</objective>

<depends_on>Phase 1</depends_on>

<!-- Repeat task structure from Phase 1 -->

</phase_2>

---

# Prohibited Patterns

### Never Use These:
1. ❌ `unimplemented!()` or `todo!()` in library code (tests/examples OK)
2. ❌ "Temporary" hacks or workarounds
3. ❌ Methods that do nothing: `fn foo() { /* no-op */ }`
4. ❌ Compatibility shims for "old code" in new implementations
5. ❌ Bypassing type safety "just to make it compile"

**Instead**: Stop, explain the problem, present options, and wait for guidance.

---

# Proposed File Structure

<file_structure>
```
src/
├── module/
│   ├── mod.rs
│   ├── new_file.rs      # NEW
│   └── existing_file.rs # MODIFIED
```
</file_structure>

---

# Verification Commands

```bash
# Build
cargo build -p crate_name

# Test
cargo test -p crate_name

# Lint
cargo clippy -p crate_name

# Format check
cargo fmt --check

# Run example
cargo run -p example_name
```

---

# Commit Message Format

Use conventional commits:

```
feat: Add [feature description]

[Optional body explaining what and why]

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>
```

Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`
