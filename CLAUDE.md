# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## ⚠️ LICENSING NOTICE ⚠️

StreamLib is licensed under the **Business Source License 1.1** (BUSL-1.1).

**When implementing features:**
- All new Rust files must include the copyright header (see existing files)
- Do NOT suggest MIT, Apache, or other licenses for this codebase
- Commercial use restrictions are intentional and must remain
- Do NOT modify license files without explicit approval

**Copyright header for new files:**
```rust
// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1
```

See [LICENSE](LICENSE) and [docs/license/](docs/license/) for full terms.

---

## 🚨 CORE OPERATING PRINCIPLES — READ FIRST 🚨

**Claude Code operates as a collaborator with the user, who is the principal architect. Think before you add; reuse before you create.**

### The StreamLib Engine Model

StreamLib is built like a game engine: a small set of **core systems** are reused across the entire codebase. The RHI is the canonical example — all GPU work in every platform, codec, and processor flows through `VulkanDevice` and `GpuContext`. There are NOT multiple ways to allocate GPU memory, submit to a queue, or create a texture. There is ONE way, and everything uses it.

Past models repeatedly created parallel abstractions without reading existing code, producing module and crate sprawl where the same concern was solved N different ways. **Do not do this.** Before introducing any new abstraction, you must prove no existing core system already covers the concern — and if one does, extend it rather than build a parallel.

### Plugin Distribution Model — the cross-repo dream

The engine is a pure substrate. It ships with **no processors, no schemas, no `streamlib.yaml` of its own** — `Runner::new()` starts with an empty registry, and every processor / schema / link is contributed by `.slpkg` packages loaded at runtime via `runtime.add_module(ident)` (or `runtime.add_module_with(ident, ModuleResolverStrategy::...)` when the caller needs to pin a strategy explicitly).

**The target distribution shape**: a plugin author in a completely separate GitHub repo, building on a completely different machine with a different rustc version and different transitive Cargo dep graph, can publish a `.slpkg` file. A streamlib host (e.g. tatolab/drone-racer, or any third-party host that Cargo-deps `streamlib`) loads that `.slpkg` at runtime via `dlopen` and the plugin's processors register cleanly into the host's runtime graph. The plugin author and the host author **never coordinate toolchains**.

What makes this work: every type that crosses the plugin ABI is `#[repr(C)]`, with its byte layout pinned by a layout regression test in `streamlib-plugin-abi`. The plugin and the host must agree on:

- Target triple (`x86_64-unknown-linux-gnu`, etc.) — for the obvious reasons
- `streamlib-plugin-abi` version — the C-ABI vtable contract
- `streamlib-consumer-rhi` version — the consumer-side carve-out (PluginAbiObject texture / buffer / device types)

They do NOT need to agree on:

- rustc version
- Cargo dep graph resolution
- Feature flags on shared transitive deps
- Optimization profile

**Where the dream currently leaks**: Phase D (#906) and Phase C2 (#905) shipped some FullAccess methods that return `Arc<HostInternalType>` via `Arc::into_raw` / `Arc::from_raw` raw-pointer transit. Those code paths DO require rustc-version coupling because the host-internal types aren't `#[repr(C)]`. Issue #917 closes that gap by refactoring the affected return types into `#[repr(C)] { handle, vtable }` PluginAbiObject pairs (mirroring `RhiCommandQueue` / `CommandBuffer`). After #917 lands, no plugin ABI return type leaks host layout, and the cross-repo dream is fully real.

**What's NOT in scope** (deferred to its own milestone): a hosted marketplace / registry / index. Today's distribution model is "package author hands you an `.slpkg` directly, or you clone the repo and run `streamlib pack` yourself". A managed registry where you'd run `streamlib install @tatolab/camera` is a separate future deliverable.

**Implication for new architectural work**: any plugin ABI surface added in this codebase MUST be `#[repr(C)]` end-to-end. If you're tempted to return `Arc<SomeHostInternalType>` from a cdylib-callable method, stop — the answer is to expose the type as a PluginAbiObject. The dormant `clone_*` / `drop_*` slots already present on the FullAccess vtable are infrastructure for this. The "rustc-version coupling stays" framing that previously appeared in the All-Dynamic Package Loading milestone body was a permissive fallback while PluginAbiObject coverage was incomplete; it's superseded as of 2026-05-22.

### Before Creating Any New Abstraction

"Abstraction" here means: a new trait, struct, helper method, utility function, or module intended to be reused across more than one call site.

1. **Search first.** Use Grep / Glob / Agent(Explore) to find existing types, traits, or modules that already solve the concern. Typical core systems to check: `vulkan/rhi/`, `core/context/`, `core/processors/`, `core/pubsub.rs`, `rhi.rs` in sub-crates.
2. **Prefer extending a core system** (adding a method to an existing trait or struct) over creating a new one. The RHI, GpuContext, and pubsub are deliberately central — grow them, don't route around them.
3. **Evaluate benefits vs drawbacks explicitly.** Write down: what problem does this solve that existing abstractions don't, what is the coupling cost, is there a simpler solution (inline the logic, duplicate at two call sites, pass a closure).
4. **If you still need a new abstraction**, propose it before implementing:
   - **Why**: the problem and why existing systems don't cover it
   - **What**: the new trait/struct/module and its shape
   - **Changes**: what existing code would change
   - **Alternatives considered**: inline, duplicate, extend existing — and why each was rejected
   - **Risks**: coupling, lifetime, thread-safety, API surface
5. **Document the decision in the PR description** so reviewers know a new core-shape concern was added intentionally. If the abstraction is load-bearing (used by multiple crates or platforms), add a short section in the PR explaining where it fits in the engine model.

Minor helpers within a single module, bug-fix-scoped private functions, and extensions to existing traits generally do not require this process — but still search first and default to the smallest change that works.

### Production-grade by default

StreamLib is an engine, not an application. The "ship the smallest thing that works, refactor when needed" defaults that suit application codebases are wrong-shaped here. Engine work biases toward production-grade by default; lighter alternatives are scope-cuts that need a reason, not the starting point.

The principles:

- **Core systems have many consumers.** Every adapter, platform, codec, and customer that hits a core trait will hit the same shape forever. Get it right at birth — the cost of breaking it later is multiplied across every downstream caller.
- **Type-system enforcement beats convention.** A typestate, a marker trait, or a `!Clone` privileged type costs the same to write as a comment saying "don't" — and pushes the wrong way out of the easy path at compile time. **Make the right way easy and the wrong way hard.** "Hard" — not "impossible". Some escape hatches are legitimate (raw-handle accessors for power users, unsafe extension points for 3rd-party adapters). When you find you need an escape hatch, surface it and discuss before adding it — don't smuggle one in to make a tricky case compile.
- **Bias toward supporting use-case classes, not single examples.** Real-time engines (Unreal, Bevy, Granite) serve classes — render targets, compute, video decode, audio, IPC. When designing or extending a core system, ask "what shape supports the *class* of use case this system is responsible for?", not "what's the smallest thing that makes the example in front of me work?".
- **Observability is a design-time concern, not a retrofit.** `tracing::instrument` + metric hooks at trait birth is one line per method; added later it's a refactor across every implementor. Put hooks in when the trait is born.
- **Concrete consumers are known requirements.** A future consumer is *hypothetical* only when unattested. A filed issue in the same milestone, a documented use case, an in-tree caller in a sibling file — these are *known*, and must be designed for now. The system-prompt's "don't design for hypothetical futures" rule still applies; it does *not* license under-shipping for known concrete futures.
- **Foundational work without in-repo consumers is still real work.** Engine pieces are sometimes built ahead of any current example — substrate for the project's vision, often shaped by downstream users not yet in the codebase. The absence of a known consumer is not a reason to defer functionality, ship partial shapes, or hardcode placeholders. Build to the full contract the piece is meant to carry. When the contract itself is uncertain, surface the question rather than silently scope-cutting because nobody in-repo can complain yet.
- **Engine-wide bugs get fixed at the engine layer, not in the consumer that surfaced them.** When debugging surfaces a defect in an engine primitive (RHI, `GpuContext`, runtime hooks, IPC surfaces, escalate ops) that *any* consumer of that primitive would hit — fix it at the engine layer, even when the symptom showed up in one example or processor. Examples are integration tests, not first-class consumers; an example-level bandaid removes the symptom for that example while leaving the bug in the engine for the next consumer (a future composition, a new processor, a third-party adapter) to rediscover. "I'll fix it for me; if you encounter it later, re-derive it" is a footgun. Lead recommendations with the engine-level fix; bandaids only appear when there is an explicit scope-cutting reason and the user gets the call. Restraint still applies (don't refactor neighboring systems just because the engine is now in scope), but the bar is: defect at engine layer ⇒ fix at engine layer.
- **Ship complete; deferrals get tickets, not comments.** If a code path is required for the use-case class or foundational contract the engine work is shipping, make it work now. If something is genuinely blocked or scope-cut, file an issue with the unblock condition — don't bury the deferral as a code comment. Comments rot silently; tickets get triaged.

What this means concretely when designing or extending a core system (RHI, IPC, processor model, public ABI, surface adapters, escalate ops):

- Comprehensive error taxonomy at trait birth — named `enum` variants with actionable context, no `()` errors, no panic-on-internal-bug.
- `tracing` instrumentation on every public entrypoint.
- ABI version constants on every cross-process / cross-crate / cross-language boundary.
- Conformance / contract tests as first-class artifacts whenever a trait will have multiple implementors (in-tree or 3rd-party).
- Layout regression tests for every `#[repr(C)]` type that crosses a language boundary, in every language that mirrors it.
- Documentation per the autocomplete-focused doc rules below — terse, but every public type has one.

What stays the same as the system-prompt defaults:

- Don't add validation for impossibilities. Type-system invariants don't need runtime checks.
- Don't keep half-finished implementations or `todo!()` in library code.
- Don't introduce abstractions that solve no problem the engine is responsible for ("just in case") — but DO introduce abstractions that solve the *class* of problem a core system addresses, when the class is documented.

When presenting design choices for engine work, recommend the production-grade option as **the right way** by default. Only present a lighter alternative when there's a specific reason to scope-cut (out-of-band time pressure, a research-gated unknown, etc.) — and call out the scope-cut explicitly so the user can confirm.

### Other Guardrails

1. **No silent DRY refactors.** Duplicate code across unrelated call sites is fine. Extracting a helper is fine IF it replaces real duplication in a core system AND the extraction is mentioned in the PR. Don't refactor out of aesthetic preference alone.

2. **No auto-fixing on the side.** If `cargo check`, `cargo test`, or `cargo clippy` surfaces problems outside the current task, report them — do not silently fix unrelated issues in the same branch.

3. **Scope discipline.**
   - Modify files within the task's scope. Files outside scope: ask before editing.
   - Simple in-method fixes: allowed.
   - Rewriting a file or large section: summarize the plan first.
   - Adding new public API or changing existing signatures: get approval.
   - **Engine-core changes** (RHI, IPC wire format, processor model, public ABI crates, escalate ops): a written plan is required, not optional. The plan covers trait shape, error taxonomy, observability hooks, polyglot mirrors, and tests, before any code lands.

4. **No bad patterns left behind on engine changes.** When an engine change establishes a new canonical way to do something, sweep the repo and migrate every consumer of the old pattern in the same PR — Rust + Python + Deno code, examples, processors, tests, docs, learnings. This is an explicit exception to "files outside scope: ask before editing." The codebase must always encourage the right pattern at every read site, because future AI agents (and humans) read existing code to learn patterns; if half the codebase uses the old shape, the old shape survives forever in onboarding. Examples especially matter: agents treat example code as canonical "how to use the engine." Update every doc/learning that endorsed the old pattern with a strikethrough + dated note per the markdown editing rules. "Scope discipline" still applies (don't refactor *unrelated* systems just because the engine moved) — but every consumer of the old pattern is *related*, not unrelated, when the engine moves.

### Work Tracking

**GitHub is the source of truth for work in this repo.** Milestones group deliverables; issues track individual tasks within each milestone. Amos is the local cache + AI-context layer — it reflects GitHub, never the other way around.

**Picking up the next task:** invoke the `/amos:next` skill (or just say "continue" / "next task" / "what's next"). It finds the next ready issue in the focused milestone, pulls the issue body from GitHub, auto-loads any matching `.claude/workflows/<label>.md` for the issue's labels, and walks the execution protocol. To set the focused milestone, run `/amos:focus <title>` — `amos milestones` lists candidates.

**Issues are goals, not specs.** An issue captures the *intent* of the work — the problem to solve, why it matters, and roughly how done looks. The specific exit-criteria checkboxes, file paths, suggested orderings, and AI Agent Notes inside an issue body are the best understanding *as of when the issue was filed*; they go stale fast as code lands, dependencies close, or the surrounding architecture shifts. **When you pick up a task, treat the issue as the goal but research current state before locking the plan.** Re-read the referenced files, check whether referenced code still exists in the shape claimed, verify whether listed follow-ups have already been filed, confirm whether flagged "defects" are still defects. Then announce a *fresh* task plan that supersedes the issue body where evidence has shifted — and update the issue body in place per the markdown-editing rules below (strike through stale items with reasoning, don't silently rewrite). Don't be dogmatic about checking off every original criterion if the world has moved; do hit the goal.

**Writing issues:** every new issue follows the template in @docs/issue-template.md — Description / Context / Exit criteria / Tests or validation / Related / AI Agent Notes. **Issue bodies carry the architecture content for proposed work.** Architecture docs (`docs/architecture/*.md`) reflect merged-in code only and never describe upcoming changes; mermaid diagrams, BNF grammars, decision matrices, ADR-style trade-off discussions, sequenced migration plans, and any other design content for not-yet-shipped work all live in the relevant issue / milestone body until merged. Keep implementation specifics (exact file paths, test function names, suggested ordering) **out** of the issue body — those rot fast and the picker re-derives them at pickup time. The picker's job is to research current state and produce the implementation plan; the filer's job is to capture the goal AND the architecture proposal cleanly enough that a competent agent can pick the issue up cold without re-litigating the design. Cross-cutting concerns (linux, macos, polyglot, ci, frozen) are labels, not milestones. Test harnesses are their own issues. Dependency edges (`blocked by` / `blocks` / `parent`) are native GitHub relationships, not text. Use the `/amos-file` skill to draft new issues — it handles the template, milestone inference, and relationships in one pass.

**Specialty workflows** live at `.claude/workflows/<label>.md`. Add a new one by dropping a file there and labeling relevant issues. `/amos:next` loads every matching file into context before starting work. Current workflows:
- `.claude/workflows/ci.md` — for `ci`-labeled issues (negative test + green baseline evidence required)
- `.claude/workflows/video-e2e.md` — for encoder/decoder/display changes (scenario matrix + PNG Read-tool check required)
- `.claude/workflows/macos.md` — for macOS-platform work (cross-compile verification required on Linux)
- `.claude/workflows/polyglot.md` — for Python/Deno SDK or escalate IPC changes (all three runtimes must stay in sync)
- `.claude/workflows/research.md` — for research-labeled issues (doc deliverable, no code)

**Prefer the Task system over todos** for in-session multi-step work and plan mode implementations.

---

## ⚠️ CRITICAL IMPLEMENTATION INSTRUCTIONS FOR CLAUDE CODE ⚠️

This document is a **complete implementation specification**. You MUST follow it exactly as written.

### Rules for Implementation:

1. **NO DEVIATIONS**: Do not make design decisions, simplifications, or "improvements" without explicit approval
2. **ASK BEFORE CHANGING**: If you encounter:
   - Ambiguity in the spec
   - Something that seems "too complex"
   - Uncertainty about implementation details
   - Desire to refactor or simplify
   - **STOP IMMEDIATELY** and ask for clarification
3. **IMPLEMENT AS-IS**: Follow the code examples verbatim, including:
   - Exact struct field names
   - Exact method signatures
   - Exact error handling patterns
   - Exact comments and documentation
4. **VERIFY AGAINST SPEC**: Before completing any task:
   - Re-read the relevant section
   - Confirm your implementation matches the spec exactly
   - Check that you haven't added "helpful" changes
5. **REPORT DEVIATIONS**: If you must deviate (e.g., Rust syntax errors in spec), report the issue and propose the minimal fix

### This System is Critical:
- Powers real-time audio/video processing
- Must handle dynamic graph modifications safely
- Memory leaks or crashes are unacceptable
- Performance regressions will break production workloads

### When in Doubt:
**STOP. ASK. WAIT FOR APPROVAL.**

### Naming Standards - NON-NEGOTIABLE

The naming in this codebase is **empirically validated** to improve AI coding accuracy. These names were designed by humans after extensive review. **Do NOT suggest shorter names.**

#### Core Principle
Names should be understood with **ZERO context**. An AI agent (or developer) who just woke up with amnesia should understand what something does from the name alone.

#### What Makes a Good Name
1. **Encodes relationships**: Where it comes from, where it goes
2. **Encodes role**: What it DOES in the system, not what it IS technically
3. **Explicit direction**: `FromUpstream`, `ToDownstream`, `Input`, `Output`
4. **No generic words alone**: Never just `Inner`, `State`, `Manager`, `Handler`, `Context`

#### Validated Examples (DO NOT SHORTEN)
```rust
// ✅ CORRECT - explicit, self-documenting
LinkOutputDataWriter         // writes data from a link output
LinkInputDataReader          // reads data for a link input
LinkInputFromUpstreamProcessor   // binding FROM upstream TO this input
LinkOutputToDownstreamProcessor  // binding FROM this output TO downstream
LinkOutputToProcessorMessage     // message sent from link output to processor
add_link_output_data_writer()    // adds a data writer to a link output
set_link_output_to_processor_message_writer()  // 43 chars is FINE

// ❌ WRONG - too short, requires context
Writer, Reader, Producer, Consumer
Connection, Binding, Handle
ctx, mgr, conn, buf, cfg
```

#### The Test
Ask: "If I saw this name 200 lines away from its declaration, would I know exactly what it is?"
- `LinkOutputDataWriter` → Yes, it writes data from a link output
- `Writer` → No, writer of what? Where?

#### When Naming New Things
Use the `/refine-name` command to get suggestions that follow this pattern. The command will suggest MORE explicit names, never shorter ones.

### Prohibited Patterns - Never Use These:
1. ❌ `unimplemented!()` or `todo!()` in library code (tests/examples are OK)
2. ❌ "Temporary" hacks or workarounds
3. ❌ Methods that do nothing: `fn foo() { /* no-op */ }`
4. ❌ Compatibility shims for "old code" in new implementations
5. ❌ Bypassing type safety "just to make it compile"
6. ❌ Reshaping library code to satisfy a test — code and architecture drive tests, not the reverse. If a test is failing because the code changed intentionally, update the test. If a test reveals a real defect, fix the defect.
7. ❌ Writing tests that paper over broken APIs — if you have to mock half the system or ignore errors to get a test green, the test is lying. A test that passes against a broken API is worse than no test.
8. ❌ `gpu_limited_access().escalate(...)` from inside ANY FullAccess lifecycle body — `setup`, `teardown`, Manual-mode `start`, or Manual-mode `stop`. The engine wraps every cdylib-resident dispatch of those four in `with_cdylib_scope` (#1072 introduced it for setup/teardown; #1075 extended to start/stop for symmetry), which holds the escalate gate around the body. Inner `.escalate(...)` re-enters the gate on the same thread and trips `EscalateGate`'s same-thread re-entry panic. The historical sandbox contract gave every FullAccess lifecycle method direct privileged access — call `ctx.gpu_full_access().X()` directly instead. Pattern 4 (escalate) is reserved for LimitedAccess contexts (`process`, `on_pause`, `on_resume`, Reactive `start`) where the gate is NOT held by the dispatcher. See `docs/architecture/cdylib-reachability.md` for the full pattern catalog.

**Instead**: Stop, explain the problem, present options, and wait for guidance.

### Test Philosophy - CRITICAL

Tests are the **first gate in automated development**. They must give high confidence the code works before a single example is run. High-quality tests remove the need for manual validation via examples. Examples showcase features; tests prove the system works.

**When end-to-end validation is needed**, follow @docs/testing.md — it specifies which example/fixture and PNG-sampling workflow to use for each scenario (encoder/decoder vs. camera+display-only), and requires reading sample PNGs with the Read tool to confirm frame content.

**Creating, updating, and deleting tests never requires approval. Tests are standard scope for every task, AMOS node, and GitHub issue.**

When creating a task or issue, include testing goals — the types of tests needed and what conditions they cover (positive, negative, error). Not the exact code, just the intent.

#### What tests validate

- **Migrations**: Confirm the migration had the intended effect and introduced no regressions. Understand what changed, then write tests that prove the change is correct.
- **New features**: Cover positive paths, negative paths, and error/resource conditions (memory allocation failures, invalid input, concurrent access).
- **Bug fixes**: Reproduce the bug first, then confirm it's gone.

#### Test infrastructure

- **Use fixtures** — pre-recorded video (ffmpeg animated content), test audio files, binary payloads. Tests must not require a live camera or microphone to run.
- **Use virtualized sources** where hardware is needed — a virtual V4L2 camera playing fixture content, a virtual audio device. Do not write tests that only work on one physical setup.
- **Generalize across hardware** — if a test works on one GPU it must work on any GPU. Do not hardcode device names, memory sizes, or vendor-specific behavior.
- **GPU, audio, and display are available** in this environment — tests may use them but must not assume a specific configuration.
- **Keep tests lightweight** — prefer unit tests over integration tests where coverage is equivalent.

#### Test quality

- **No zero-value tests** — do not test known truths. Every test must exercise real behavior that could plausibly break.
- **Flag flaky tests** with `#[ignore]` and a comment explaining why. Do not leave flaky tests running silently — they destroy trust in the suite.
- **Flag tests with side effects** — tests that write files, create IPC services, or mutate global state must clean up. Document side effects explicitly.
- **Identify suite-degrading tests** — tests that take unexpectedly long or hang must have timeouts. Flag them if they can't be fixed.

#### Code drives tests

When functionality changes, update tests to reflect the new behavior. Never reshape library code to satisfy a test.

**When a test failure indicates a significant code change that deviates from the task goal: STOP. Summarize the issue, proposed fixes, and impact on the goal. Wait for direction before proceeding.**

### Documentation Standards - MANDATORY

Documentation should be **minimal and focused on developer experience** (autocomplete, IDE tooltips). Do NOT over-document.

#### What to Document
- **Structs/enums/traits**: One-line description of what it represents
- **Functions/methods**: Brief description, parameters only if non-obvious
- **Public fields**: Only if the name isn't self-explanatory

#### What NOT to Document
- ❌ File-level `//!` module docs (architecture explanations rot fast)
- ❌ `# Example` sections with code blocks
- ❌ `# Usage` sections
- ❌ `# Performance` sections
- ❌ ASCII diagrams or flowcharts
- ❌ Design rationale or "how this fits into the system"
- ❌ Historical context
- ❌ Verbose parameter descriptions for obvious params

#### Style Rules
1. **One line preferred** - if you need multiple paragraphs, it's too much
2. **Use intra-doc links** for type references: `[`TypeName`]` not `` `TypeName` ``
3. **No examples in docs** - examples belong in `examples/` directory
4. **Brief parameter docs** - only for non-obvious parameters

```rust
// ✅ CORRECT - minimal, useful for autocomplete
/// Processor node in the graph.
pub struct ProcessorNode { ... }

/// Connect two ports.
pub fn connect(&mut self, from: impl IntoLinkPortRef, to: impl IntoLinkPortRef) -> Result<Link>

/// Convert audio frame to a different channel count.
pub fn convert_channels(frame: &AudioFrame, target_channels: AudioChannelCount) -> AudioFrame

// ❌ WRONG - too verbose
/// Convert audio frame to a different channel count.
///
/// # Channel Conversion Rules
/// - Upmixing: Duplicate channels or zero-fill
///   - Mono → Stereo: duplicate to both channels
/// ...
/// # Example
/// ```rust
/// let stereo = convert_channels(&mono_frame, AudioChannelCount::Two);
/// ```
```

#### Verification
Run `cargo doc -p streamlib --no-deps` - fix any unresolved link warnings.

---

## Conventions

### Error Handling
- Use `Error` enum from `streamlib::core::error`
- Return `Result<T>` from all fallible operations
- Prefer `?` operator over `.unwrap()` in library code
- `.unwrap()` acceptable in examples and tests

### Code Organization
- **Platform-agnostic code**: `libs/streamlib-engine/src/core/`
- **macOS/iOS code**: `libs/streamlib-engine/src/apple/`
- **DO NOT** use `#[cfg]` inside platform-specific directories (already conditionally compiled)

### Dependencies
- **Git dependencies must be pinned** with `rev = "<commit sha>"` (or `tag = "..."`). Never use a bare `git = "..."` or `branch = "..."` — Cargo resolves those against the remote's current HEAD, so fresh clones drift out of sync with the lockfile and stop compiling. This applies to every `Cargo.toml` in the workspace, including `[patch.crates-io]` entries.

### Dependency resolution & distribution — unified Gitea registry

**Decided architecture, validated by POC, in active migration** (the "Gitea
Package Registry" milestone). Documented ahead of full implementation on
purpose — follow it; do **not** reimplement resolution a different way. Full
model + which issue finalizes each piece: @docs/architecture/gitea-registry-distribution.md.

Every StreamLib-authored **or customized** artifact is published to a single
self-hosted **Gitea** instance under the **`tatolab`** org and resolved **by
version** — never by relative `path` or git `[patch]` in anything a consumer
sees:

- **SDK libraries** (rust `streamlib` crate chain, python pkg, deno module) →
  Gitea's cargo / pypi / npm registries.
- **Packages** (polyglot streamlib packages) → **source-only `.slpkg`** via
  `streamlib pack` → Gitea's generic registry.
- **`streamlib.yaml` schema deps** (e.g. `@tatolab/escalate`) → resolved from
  the generic registry (schema-package `.slpkg`), not a dev path patch.
- **Truly-external untouched deps** (serde, tokio, …) keep resolving from
  their normal public registries.

Rust internal cross-crate deps use `{ path = "../foo", version = "x.y",
registry = "gitea" }` — the `path` is a dev-only affordance cargo strips from
the published manifest (standard workspace-publish pattern); `registry =
"gitea"` is required or cargo defaults to crates.io. A local engine change is
published as a `0.4.x-dev.N` version the consumer bumps to — **never** a new
relative-path dep or `[patch]`. Don't introduce new relative-path / git-`[patch]`
cross-crate deps. The monorepo still builds itself in-place (the dev `path`
wins locally); publishing is a release step.

### Vulkan RHI Boundary — ABSOLUTE RULE

**NOTHING outside the RHI may touch Vulkan APIs directly.** "The RHI" here means `libs/streamlib-engine/src/vulkan/rhi/` (host-side) and `libs/streamlib-consumer-rhi/` (consumer-side carve-out, #560) — together they own every `vulkanalia` call in the workspace. No processor, utility, codec wrapper, or any other code may call `vulkanalia::Device`, `vkAllocateMemory`, `vkCreateImage`, or any Vulkan function without going through one of those two crates. This is non-negotiable. (`ash` is fully removed from the workspace per #252; never reintroduce it.)

The boundary is enforced in CI by `cargo xtask check-boundaries` (see `xtask/src/check_boundaries.rs` and `.github/workflows/check-boundaries.yml`). The check fails any PR that reintroduces `ash`, reaches for raw `vulkanalia` outside the RHI / consumer-rhi / adapter / codec crates (in `.rs` imports OR in Cargo.toml deps), makes a cdylib or adapter crate depend on the full `streamlib` crate at runtime, calls a privileged Vulkan primitive (`vkAllocateMemory`, `vkGetMemoryFdKHR`, `vkCreateComputePipelines`) outside the RHI, or declares any `vulkanalia` / `vulkanalia-sys` / `vulkanalia-vma` dep that bypasses `[workspace.dependencies]` (the `tatolab/vulkanalia` fork is the single source of truth — direct version specs in member crates can silently pull crates.io upstream and lose the VMA 3.3.0 patch). Allowlists for legitimate exceptions are explicit and carry per-entry rationale; the right move when a check trips is almost always to extend the offending file to ride the RHI / consumer-rhi shape, not to add an allowlist entry.

The RHI is the **single gateway** to all GPU operations on Linux. Like Unreal Engine's RHI, it gives the runtime absolute control and traceability over every GPU resource.

The RHI is split across **two crates** along a privilege axis:
- **`streamlib::vulkan::rhi`** (in `libs/streamlib-engine/src/vulkan/rhi/`) — the host-side RHI. Owns `HostVulkanDevice` (allocator + queue matrix + swapchain extensions), `HostVulkanTexture`, `HostVulkanBuffer`, `HostVulkanTimelineSemaphore`, `VulkanComputeKernel`, the modifier probe, etc.
- **`streamlib-consumer-rhi`** (in `libs/streamlib-consumer-rhi/`, #560) — the consumer-side carve-out. Owns `ConsumerVulkanDevice`, `ConsumerVulkanTexture`, `ConsumerVulkanBuffer`, `ConsumerVulkanTimelineSemaphore` plus the `VulkanRhiDevice` / `DevicePrivilege` / `VulkanTextureLike` / `VulkanTimelineSemaphoreLike` trait machinery surface adapters use to abstract over the device flavor. Subprocess cdylibs (`streamlib-python-native`, `streamlib-deno-native`) depend on `streamlib-consumer-rhi`, NOT the full `streamlib`, so the FullAccess capability boundary is enforced by the type system: a cdylib's dep graph excludes `streamlib` and therefore physically cannot reach `HostVulkanDevice` etc.

#### The boundary:
- **`vulkan/rhi/`** (HostVulkanDevice, HostVulkanTexture, HostVulkanBuffer, VulkanVideoEncoder, etc.) — MAY call Vulkan APIs. All GPU memory allocation goes through HostVulkanDevice via `vulkanalia-vma`.
- **`streamlib-consumer-rhi`** (ConsumerVulkanDevice and friends) — MAY call Vulkan APIs, restricted to the import-side carve-out from `docs/architecture/subprocess-rhi-parity.md`: DMA-BUF FD import + bind + map, layout transitions on imported handles, sync wait/signal on imported timeline semaphores. No allocator, no kernel construction, no swapchain.
- **`core/context/`** (GpuContext, TexturePool, PixelBufferPoolManager) — wraps the RHI with pooling, caps, and lifecycle management. This is what processors see.
- **Processors** (`core/processors/`, `linux/processors/`, `apple/processors/`) — ONLY interact with GpuContext. They acquire/release resources from managed pools. They NEVER import from `vulkanalia`, `vk`, or `vulkan/rhi/` directly.

#### Violations of this rule:
```rust
// ❌ WRONG — processor importing Vulkan types
use vulkanalia::vk;
use crate::vulkan::rhi::VulkanDevice;

// ❌ WRONG — processor doing raw allocation
let memory = unsafe { device.allocate_memory(&alloc_info, None) };

// ❌ WRONG — processor creating Vulkan images
let image = unsafe { device.create_image(&image_info, None) };

// ✅ CORRECT — processor uses GpuContext
let (id, buffer) = ctx.gpu.acquire_pixel_buffer(width, height, format)?;
let texture = ctx.gpu.acquire_texture(&desc)?;
```

#### Compute kernels — single canonical abstraction

All GPU compute work goes through `VulkanComputeKernel` plus the public
`ComputeKernelDescriptor` / `ComputeBindingSpec` types in `core::rhi`.
**Never hand-roll a descriptor set, descriptor pool, command buffer,
fence, or pipeline layout for a compute shader.** Add new kernels by
declaring their bindings as data and calling `GpuContext::create_compute_kernel`.
SPIR-V reflection (via `rspirv-reflect`) validates the declared layout
against the shader at construction. See @docs/architecture/compute-kernel.md
for the full recipe.

Same shape applies to graphics-pipeline work
(@docs/architecture/graphics-kernel.md) and ray-tracing work
(@docs/architecture/ray-tracing-kernel.md). Each pipeline kind
ships its own kernel type with the same SPIR-V-reflected /
descriptor-managed / pipeline-cached invariants — RT additionally
owns a shader-binding-table layout and depends on
`VulkanAccelerationStructure` for BLAS/TLAS construction. Pick the
kernel that matches the pipeline kind; never hand-roll a parallel
shape.

#### TextureRegistration — single canonical per-surface state

Every per-surface lifecycle field lives on `TextureRegistration`,
keyed by `surface_id` in `GpuContext::texture_cache`. Producers
declare state at registration (`register_texture_with_layout`);
consumers read it via `resolve_videoframe_registration` and update on
transitions. **Never create a parallel `HashMap<surface_id, ...>` for
new per-surface metadata** — extend the `TextureRegistration` record
instead. **Never bypass the registration with descriptor-side claims
that don't match reality** (the bug class #616 fixed). The shape
mirrors `streamlib-adapter-vulkan::SurfaceState::current_layout`
lifted from adapter-scope to engine-scope; if you're tempted to
duplicate that pattern in a third place, stop and read
@docs/architecture/texture-registration.md before deciding.

When in doubt about whether a new field belongs on `TextureRegistration`
versus elsewhere (`Videoframe` IPC for per-frame state; adapter
`SurfaceState` for adapter-internal state; RDG #631 for declared-usage
hints), read the doc end-to-end first — the boundaries are deliberate.

#### Texture rings — single canonical abstraction

Every decode-output / CPU-upload-style hot path that needs a
small ring of rotating output textures rides
`TextureRing` plus the public
`GpuContextFullAccess::create_texture_ring` /
`GpuContextLimitedAccess::copy_pixel_buffer_to_texture` pair.
**Never hand-roll a `Vec<Texture>` + rotating index inside a
consumer for this use case** — the engine helper pre-allocates
on FullAccess at setup, hands back an `Arc<TextureRing>` that
the consumer rotates via `acquire_next()` from `process()`,
and the companion Limited primitive writes CPU-staged bytes
into a slot without escalating. `MAX_FRAMES_IN_FLIGHT = 2`
slots is the standard depth. See @docs/architecture/texture-ring.md
for the recipe (separate sub-recipes for CPU-upload consumers
and GPU-native producers — the ring shape is the same; only the
slot-write primitive differs).

GPU-native producers that hand-roll their own ring today
(camera) are not in scope to migrate retroactively, but new
producers should ride the helper from day 1.


### Custom Commands

- `/refine-name <current_name>` - Get MORE explicit naming suggestions (never shorter)

---

## Architecture documentation discipline

**`docs/architecture/*.md` describes the current known state of the
system, subject to staleness or drift.** A reader who has never seen
the code should be able to skim an arch doc and walk away with a
working mental model of how the system is shaped right now —
identifier grammars the validators enforce, manifest formats the
parsers accept, invariants the `compile_fail` doctests lock, files
that exist on disk. Drift is expected and tolerated: a doc claim is
the best understanding when last edited, nothing more. Verify
against the code before relying on anything load-bearing.

What architecture docs are **not**:

- Not GitHub-aware. They don't reference issues, milestones, PRs, or
  any project-management tracker. The codebase is the only thing
  they describe; tracker references rot the moment the tracker
  changes hands.
- Not dated. No "as of YYYY-MM-DD," no "last updated," no embedded
  timestamps. Git history covers freshness — embedding the date in
  the doc just adds a second field that goes stale on every edit.
- Not authoritative. They're a convenience read; a doc that says
  "do X" is a hypothesis about the current code, not a command (per
  "Editing markdown documentation" below).
- Not enforcement. Architecture docs do not validate or gate
  functionality — code, tests, type-system invariants, and CI lints
  do that. A doc claim is never the reason something is correct.
- Not history. They don't describe superseded designs, never-merged
  proposals, or "this used to be X." The git log holds history.
- Not a roadmap. They don't describe proposed work, planned
  migrations, upcoming PR sequences, or "this will become X after
  some change lands." Forward-looking content creates
  false-current-state confusion when a future agent picks up a
  fresh branch and can't tell whether X is what the code does or
  what the code might do.

Proposed architecture lives in **issue / milestone bodies** while
in flight. Mermaid diagrams, BNF grammars, ADR-style trade-off
discussions, sequenced migration plans, decision matrices — all of
that goes in the issue body until the work merges. When the work
merges, the architecture moves into a `docs/architecture/*.md` file
describing the shipped state in current tense; the issue closes and
the doc takes over. The arch doc itself never references the
originating issue.

This applies retroactively: existing architecture docs that carry
proposed-work, GitHub-tracker, or historical-supersession content
should be cleaned up to current-known-state.

## Editing markdown documentation

All markdown in this repo — CLAUDE.md, `docs/learnings/`,
`docs/architecture/`, workflow files under `.claude/workflows/`,
research docs — is **living documentation**. Future agents are
encouraged to validate, update, critique, and prune as evidence
shifts. Treat these files academically, not dogmatically: older
content was true at the time it was written, and reality may have
moved on.

When editing any `*.md`:

1. **Use Opus.** Markdown edits are research work — Sonnet or Haiku
   may not hold enough context to weigh the trade-offs and
   preserve nuance.
2. **Show your work.** PR description / commit body must include
   the evidence that drove the change: command output, file paths,
   spec references, observations. "I think this is wrong" without
   evidence is not enough.
3. **Preserve disagreement, don't silently overwrite.** When you
   supersede content, annotate it rather than removing it cleanly:

   ```markdown
   > ~~Original claim that X holds.~~ — Superseded YYYY-MM-DD by
   > <evidence>. <One or two sentences on why the original is no
   > longer right>.
   ```

   Outright deletion is allowed when content is provably wrong or
   no longer relevant; in that case, leave a one-line marker in
   the surrounding text ("section on X removed YYYY-MM-DD —
   <reason>") so a future reader doesn't re-derive the same dead
   end. Crossed-out content carries learnings of its own ("don't
   go down this path; an agent tried it and it didn't work because
   Y") — those are valuable and should be preserved.
4. **Don't be dogmatic.** Re-derive conclusions when the stakes
   are non-trivial. A doc that says "do X" is a hypothesis for
   your current situation, not a command.

These rules apply to every doc in this repo, including this one.

## Hard-won learnings (look these up when triggered)

These docs capture surprising, non-obvious behavior — driver bugs,
library quirks, allocation patterns. Look them up when the trigger
condition matches what you're seeing.

### How to treat them

Learnings follow the general
[Editing markdown documentation](#editing-markdown-documentation)
rules above; this subsection is a learnings-flavored restatement.
They are **notes from past-me to current-me** — a conversation
across sessions, not a spec. Treat them the way any engineer treats
their own older notes: useful prior context, **not authority**.

- **Be skeptical.** A learning is a snapshot of what was true when it
  was written. Drivers update, code refactors, assumptions shift. If
  reality disagrees with a learning, trust what you observe now.
- **Always be learning.** You can discover new things a learning
  missed, realize the problem was framed wrong, or find a simpler /
  better fix. Prefer the current understanding over the recorded one
  when they conflict.
- **Edit freely, preserve disagreement.** A learning can be updated,
  rewritten, split, merged, or deleted whenever you have evidence
  it's out of date, wrong, incomplete, too narrow, too broad, or
  simply no longer relevant. When you do, follow the general rules
  above — annotate superseded content, leave deletion markers
  explaining why, preserve the dead-end as its own learning.
- **Don't follow them blindly.** A learning telling you "do X" is a
  hypothesis for your current situation, not a command. Verify the
  trigger still matches, verify the prescribed fix still applies,
  and re-derive the conclusion from first principles when the stakes
  are non-trivial.

### What makes a good learning

- **Specific enough to be useful** — name the symptom precisely (exact
  error string, exact VUID, exact failure pattern) so a search-match
  fires when it should.
- **Not so specific that it rots** — tie the lesson to a concept,
  constraint, or invariant, not to a single line number or a single
  file path. If the fix is "chain `VkExportMemoryAllocateInfo` through
  VMA pools instead of the global allocator config," that's portable;
  if it's "edit line 137 of `vulkan_device.rs`," it will be wrong
  within a month. Link to the relevant files for orientation, but
  make the lesson hold even after the surrounding code moves.
- **Say *why*, not just *what*** — the underlying driver/library/spec
  constraint is what survives refactors; the code that happened to
  trip over it is not.

If you're writing a new learning, aim for something that would still
make sense if the surrounding files were renamed or restructured.

- @docs/learnings/nvidia-dma-buf-after-swapchain.md — `VK_ERROR_OUT_OF_DEVICE_MEMORY`
  from `vmaCreateImage`/`vkAllocateMemory` on NVIDIA Linux when a swapchain
  has been created. NOT real OOM.
- @docs/learnings/nvidia-opaque-fd-after-swapchain.md — Same NVIDIA cap as
  DMA-BUF but for the OPAQUE_FD path used by CUDA / OpenCL interop. The
  engine pre-warms every export-capable VMA pool at
  `HostVulkanDevice::new()`; OPAQUE_FD probes are *retained as long-lived
  sentinels* (issue #637) because the compositor doesn't keep an OPAQUE_FD
  allocation alive in the kernel like it does for DMA-BUF, so the per-handle-
  type kernel state can decay. Consumers don't need to (and shouldn't) pre-warm.
- @docs/learnings/nvidia-egl-dmabuf-render-target.md — Linear DMA-BUFs on
  NVIDIA Linux are sampler-only (EGL `external_only=TRUE`); FBO color
  attachments require a tiled DRM modifier from `eglQueryDmaBufModifiersEXT`.
  Read before importing a DMA-BUF as a GL render target.
- @docs/learnings/vma-export-pools.md — Mixing DMA-BUF exportable and
  non-exportable VMA allocations. Read before adding/changing
  `pTypeExternalMemoryHandleTypes` or any export memory configuration.
- @docs/learnings/vulkan-frames-in-flight.md — Per-frame Vulkan resources
  (semaphores, command buffers, descriptor sets, render-target rings) must
  be sized to `MAX_FRAMES_IN_FLIGHT = 2`, NOT `swapchain.images.len()`.
  Read before sizing any per-frame resource.
- @docs/learnings/camera-display-e2e-validation.md — Validating
  camera→display end-to-end via virtual camera + AI-readable PNG sampling.
  Read before trying to test GPU pipeline changes (mocked unit tests
  often miss driver bugs).

- @docs/learnings/vulkanalia-empty-slice-cast.md — Cryptic type
  inference error (`cannot satisfy _: Cast`) when passing `&[]` to
  vulkanalia Vulkan methods. Fix: explicit cast `&[] as &[vk::MemoryBarrier]`.
  Read before writing any `cmd_pipeline_barrier` or similar call with
  empty barrier arrays.
- @docs/learnings/pubsub-lazy-init-silent-noop.md — Test hangs
  indefinitely with no error output. PUBSUB silently no-ops (subscribe
  buffers, publish drops) without `init()`. Read before writing any test
  that uses PUBSUB events (shutdown, reconfigure) outside a full
  `StreamRuntime`.
- @docs/learnings/cdylib-make-borrow-cached-fields.md — Cdylib
  pipeline runs end-to-end with zero errors / zero panics / zero
  validation complaints but produces all-zero / black output. Trigger:
  a host-side `make_*_borrow` helper constructed a `ManuallyDrop`'d
  PluginAbiObject borrow with the cached POD fields zeroed; host-side code
  then read a cached field off the borrow (`.width()` / `.byte_size()`
  / etc.) and got zero. Read before adding a new host wrapper that
  reconstructs a borrowed PluginAbiObject from a `*const c_void` handle.
- @docs/learnings/cross-process-vkimage-layout.md — Cross-process
  `VkImage` layout coordination. `VkImageLayout` is independent state
  per `VkDevice` by Vulkan spec — no shared mutable tracker. The
  consumer's first barrier with `oldLayout = <producer's layout>`
  trips VUID-VkImageMemoryBarrier-oldLayout-01197 against a freshly-
  imported `VkImage`. Fix: pair producer-side QFOT release
  (`dstQueueFamily = VK_QUEUE_FAMILY_EXTERNAL`, core Vulkan 1.1) with
  consumer-side QFOT acquire (`srcQueueFamily = VK_QUEUE_FAMILY_EXTERNAL`
  chaining `VkExternalMemoryAcquireUnmodifiedEXT` from the optional
  `VK_EXT_external_memory_acquire_unmodified` extension). Falls back
  to bridging `UNDEFINED → target` (content discard permitted by
  spec, preserved in practice on every modern Linux Vulkan driver)
  when the extension is missing. To the best of our current
  knowledge as of 2026-05-03, NVIDIA Linux is not shipping
  `acquire_unmodified` even in betas — so the bridging fallback is
  structurally permanent on NVIDIA, with the QFOT path reserved for
  Mesa. Read before consuming an imported `VkImage` on the host or
  in a cdylib.
- @docs/architecture/adapter-runtime-integration.md — Two IPC seams
  (surface-share FD lookup, escalate IPC) already exist for handing
  host-allocated adapter resources to subprocess customers. The doc
  records which adapter rides which today and why. To the best of our
  current knowledge GPU adapters (Vulkan / OpenGL / Skia) fit one-shot
  FD passing and cpu-readback fits per-acquire escalate, but the
  trade-offs may shift as new adapters arrive — verify against current
  code before generalizing. Read before adding a new surface adapter
  or wondering why one path was picked over another.
- @docs/architecture/subprocess-rhi-parity.md — Companion to
  adapter-runtime-integration. Where adapter-runtime is "*how* a
  subprocess obtains an adapter context", this is "*which* RHI
  patterns the subprocess re-implements once it has one." To the best
  of our current knowledge the answer is "only the import-side
  carve-out — everything else escalates"; the doc buckets each
  pattern (compute dispatch, queue mutex, frames-in-flight, modifier
  probe, validation, dual-VkDevice) and lists trip-wires that would
  shift the bucketing. Read before adding subprocess-side Vulkan code
  beyond `vkImportMemoryFdInfoKHR` + `vkBindBufferMemory` +
  `vkMapMemory`.
- @docs/architecture/texture-registration.md — Engine-wide per-surface
  lifecycle state record (`TextureRegistration`) keyed by `surface_id`
  in `GpuContext::texture_cache`. Producers declare state at
  registration; consumers read it via `resolve_videoframe_registration`
  and update on transitions. Read before adding any new per-surface
  metadata, before tracking layout state, before wondering if there's a
  better way than convention to coordinate handoff between a producer
  and a consumer through a `surface_id`. Same-process and cross-process
  consumers both work today: cross-process layout flows via three
  layers (per-frame `Videoframe.texture_layout` override → per-surface
  `current_image_layout` from surface-share IPC → default UNDEFINED),
  resolved on Path 2 by `acquire_from_foreign` (QFOT acquire when
  extensions allow; bridging `UNDEFINED → target` fallback otherwise —
  see @docs/learnings/cross-process-vkimage-layout.md). To the best
  of our current knowledge subprocess code does NOT need to construct
  `TextureRegistration` itself (the speculation tracked as #634 was
  closed without code change after research showed cross-process
  layouts are independent state machines per Vulkan spec; see the
  doc's "Why no sandbox-side mirror" section). Working rule: don't
  create a parallel engine-wide `HashMap<surface_id, ...>` alongside
  `texture_cache` — extend `TextureRegistration`. Adapter-internal
  `SurfaceState<P>` lives at a different scope and is not the failure
  mode this rule prevents.
- @docs/architecture/third-party-gpu-backends.md — Canonical shape for
  integrating a third-party GPU library (NVIDIA nvJPEG, NVDEC, OptiX
  denoiser, AMD AMF, Intel MFX) as a backend behind a streamlib
  decoder/encoder/post-processor: engine-allocates an OPAQUE_FD
  staging surface via `HostVulkanBuffer::new_opaque_fd_export*`,
  vendor library imports the FD via its own SDK
  (`cudaImportExternalMemory` for CUDA), Vulkan timeline semaphore
  carries the cross-API signal, host-side `vkCmdCopyBufferToImage`
  lands the result in a normal `TextureRing` slot consumers see via
  `surface_id`. Direction is **engine-allocates / vendor-imports**
  universally — the inverse (CUDA-allocates / Vulkan-imports) cannot
  bind a tiled `VkImage` and is the anti-pattern this doc rules out.
  Read before adding a second backend-using library — the doc names
  the trigger ("`ThirdPartyGpuCapabilities` grew a second `bool`
  field") for lifting the JPEG-shaped backend trait to an engine-tier
  `ThirdPartyGpuBackend` primitive.

Index: @docs/learnings/README.md
