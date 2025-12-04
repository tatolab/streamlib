# Claude Code Refactoring Guide

This document captures patterns that enable effective autonomous refactoring with Claude Code, based on the successful Architecture V2 refactoring.

## Anthropic Documentation References

Fetch these for the latest guidance on working with Claude Code:

- **Claude Code Overview**: https://docs.anthropic.com/en/docs/claude-code
- **Claude Code Best Practices**: https://docs.anthropic.com/en/docs/claude-code/best-practices
- **Claude Code Memory & CLAUDE.md**: https://docs.anthropic.com/en/docs/claude-code/memory
- **Prompt Engineering Guide**: https://docs.anthropic.com/en/docs/build-with-claude/prompt-engineering/overview
- **Extended Thinking**: https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking
- **Claude Agent SDK**: https://docs.anthropic.com/en/docs/claude-code/claude-agent-sdk (for programmatic usage)

## Document Structure That Works

### 1. Approval Block (Critical for Autonomous Work)

Place at the top of your refactoring document:

```markdown
<approval date="YYYY-MM-DD">
**EXPLICIT APPROVAL GRANTED**: The project owner has authorized Claude Code to work 
independently as a self-directed agent on this refactoring. All phases may be implemented 
without stopping for approval at each step.

**Context**: Brief explanation of why this is safe (feature branch, pre-production, etc.)

**Constraints**: Any hard limits (don't modify X, always run tests before commit, etc.)
</approval>
```

This prevents Claude from stopping to ask permission at every decision point.

### 2. Current State Documentation

Document the **actual** codebase state before changes:

```markdown
<current_architecture>
<structure name="ComponentName" file="path/to/file.rs">
```rust
// Actual current code, not aspirational code
pub struct CurrentThing { ... }
```

**Methods**: list actual methods
**Problems**: what's wrong with this
</structure>
</current_architecture>
```

This prevents Claude from making assumptions about what exists.

### 3. Phase-Based Task Breakdown

Break work into phases with explicit dependencies:

```markdown
<phases>
<phase id="1" name="Extract X" risk="low" breaking="no">
Brief description of what this phase does.
</phase>

<phase id="2" name="Introduce Y" risk="medium" breaking="internal">
Brief description. Depends on Phase 1.
</phase>
</phases>
```

### 4. Task Structure with Verification

Each task should have:

```markdown
<task id="1.1" name="Create Module Structure">
<instruction>
Clear, imperative instruction of what to do.
</instruction>

<files_to_create>
- path/to/new/file.rs
- path/to/another/file.rs
</files_to_create>

<example context="What this example shows">
```rust
// Concrete code example - not pseudocode
pub fn actual_function() -> Result<()> {
    // Real implementation
}
```
</example>

<verification>
```bash
cargo test -p crate_name
cargo check
```
</verification>
</task>
```

### 5. Thinking Blocks for Design Rationale

When there are design decisions, explain the reasoning:

```markdown
<thinking>
Why we're doing X instead of Y:
1. Reason one
2. Reason two

Trade-offs considered:
- Option A: pros/cons
- Option B: pros/cons (chosen because...)
</thinking>
```

This helps Claude make consistent decisions when encountering edge cases.

### 6. Prohibited Patterns

Explicitly state what NOT to do:

```markdown
### Prohibited Patterns - Never Use These:
1. ❌ `unimplemented!()` or `todo!()` in library code
2. ❌ "Temporary" hacks or workarounds
3. ❌ Compatibility shims for "old code" in new implementations
4. ❌ Skipping tests "to save time"

**Instead**: Stop, explain the problem, present options, wait for guidance.
```

### 7. Completion Checklist

End each phase with explicit success criteria:

```markdown
<completion_checklist>
- [ ] All tests pass (`cargo test -p crate_name`)
- [ ] No new clippy warnings (`cargo clippy`)
- [ ] Examples compile (`cargo build -p example_name`)
- [ ] Code committed with conventional commit message
</completion_checklist>
```

## Why This Structure Works

### For Claude's Context Window
- **Structured XML-like tags** create clear boundaries between sections
- **Concrete examples** are more useful than abstract descriptions
- **Verification steps** provide checkpoints to catch errors early
- **Phase dependencies** prevent Claude from jumping ahead incorrectly

### For Autonomous Operation
- **Approval block** eliminates permission-seeking interruptions
- **Prohibited patterns** prevent common shortcuts that cause problems
- **Current state docs** prevent assumptions about what exists
- **Todo list integration** forces incremental commits (catch errors early)

### For Quality
- **Thinking blocks** ensure consistent design decisions
- **Verification steps** catch regressions immediately
- **Completion checklists** ensure nothing is forgotten

## Integration with CLAUDE.md

Your project's `CLAUDE.md` should reference the refactoring document:

```markdown
## Active Refactoring

See [docs/ARCHITECTURE_V2.md](docs/ARCHITECTURE_V2.md) for the current refactoring plan.
This document has explicit approval for autonomous implementation.
```

## Example: Successful Refactoring Session

The Architecture V2 refactoring completed 5 phases in one session:

1. **Phase 1**: Extract Compiler from SimpleExecutor
2. **Phase 2**: Introduce Delegates (Apple-style pattern)
3. **Phase 3**: PropertyGraph with ECS (hecs)
4. **Phase 4**: Observability Layer
5. **Phase 4.5**: Remove all backwards compatibility shims

Each phase was:
- Implemented following the document structure
- Verified with `cargo test` and `cargo clippy`
- Committed with conventional commit messages
- Pushed to a feature branch

Total: ~1000 lines of new code, ~230 lines of deprecated code removed, zero manual fixes required.

## Key Success Factors

1. **Well-structured specification document** with concrete examples
2. **Explicit autonomous approval** to prevent interruptions
3. **Clean existing codebase** with consistent patterns
4. **Incremental commits** after each phase (catch errors early)
5. **MCP tools available** (rust-analyzer, GitHub) for verification
6. **Todo list tracking** to maintain progress visibility

## Template

See [docs/templates/REFACTORING_TEMPLATE.md](docs/templates/REFACTORING_TEMPLATE.md) for a blank template you can copy for new refactoring projects.
