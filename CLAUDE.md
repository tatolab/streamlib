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

What this means concretely when designing or extending a core system (RHI, IPC, processor model, public ABI, surface adapters, escalate ops):

- Comprehensive error taxonomy at trait birth — named `enum` variants with actionable context, no `()` errors, no panic-on-internal-bug.
- `tracing` instrumentation on every public entrypoint.
- ABI version constants on every cross-process / cross-crate / cross-language boundary.
- Conformance / contract tests as first-class artifacts whenever a trait will have multiple implementors (in-tree or 3rd-party).
- Layout regression tests for every `#[repr(C)]` type that crosses a language boundary, in every language that mirrors it.
- Documentation per the autocomplete-focused doc rules below — terse, but every public type has one.

What stays the same as the system-prompt defaults:

- Don't fabricate consumers. "What if someone wants X" is hypothetical until X is filed, documented, or in-tree.
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

### Work Tracking

**GitHub is the source of truth for work in this repo.** Milestones group deliverables; issues track individual tasks within each milestone. Amos is the local cache + AI-context layer — it reflects GitHub, never the other way around.

**Picking up the next task:** invoke the `/amos:next` skill (or just say "continue" / "next task" / "what's next"). It finds the next ready issue in the focused milestone, pulls the issue body from GitHub, auto-loads any matching `.claude/workflows/<label>.md` for the issue's labels, and walks the execution protocol. To set the focused milestone, run `/amos:focus <title>` — `amos milestones` lists candidates.

**Issues are goals, not specs.** An issue captures the *intent* of the work — the problem to solve, why it matters, and roughly how done looks. The specific exit-criteria checkboxes, file paths, suggested orderings, and AI Agent Notes inside an issue body are the best understanding *as of when the issue was filed*; they go stale fast as code lands, dependencies close, or the surrounding architecture shifts. **When you pick up a task, treat the issue as the goal but research current state before locking the plan.** Re-read the referenced files, check whether referenced code still exists in the shape claimed, verify whether listed follow-ups have already been filed, confirm whether flagged "defects" are still defects. Then announce a *fresh* task plan that supersedes the issue body where evidence has shifted — and update the issue body in place per the markdown-editing rules below (strike through stale items with reasoning, don't silently rewrite). Don't be dogmatic about checking off every original criterion if the world has moved; do hit the goal.

**Writing issues:** every new issue follows the template in @docs/issue-template.md — Description / Context / Exit criteria / Tests or validation / Related / AI Agent Notes. **Keep issues low-resolution by default.** State the goal, the constraints, and what "done" looks like in broad strokes; do *not* try to capture every file path, exact test name, suggested implementation order, or detailed plan in the issue body. That high-resolution detail decays as code shifts, and a future agent picking up the issue will re-derive it anyway. The picker's job is to research current state and produce the implementation plan; the filer's job is to capture the goal cleanly. Cross-cutting concerns (linux, macos, polyglot, ci, frozen) are labels, not milestones. Test harnesses are their own issues. Dependency edges (`blocked by` / `blocks` / `parent`) are native GitHub relationships, not text. Use the `/amos-file` skill to draft new issues — it handles the template, milestone inference, and relationships in one pass.

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
- Use `StreamError` enum from `streamlib::core::error`
- Return `Result<T>` from all fallible operations
- Prefer `?` operator over `.unwrap()` in library code
- `.unwrap()` acceptable in examples and tests

### Code Organization
- **Platform-agnostic code**: `libs/streamlib/src/core/`
- **macOS/iOS code**: `libs/streamlib/src/apple/`
- **DO NOT** use `#[cfg]` inside platform-specific directories (already conditionally compiled)

### Dependencies
- **Git dependencies must be pinned** with `rev = "<commit sha>"` (or `tag = "..."`). Never use a bare `git = "..."` or `branch = "..."` — Cargo resolves those against the remote's current HEAD, so fresh clones drift out of sync with the lockfile and stop compiling. This applies to every `Cargo.toml` in the workspace, including `[patch.crates-io]` entries.

### Vulkan RHI Boundary — ABSOLUTE RULE

**NOTHING outside the RHI (`vulkan/rhi/`) may touch Vulkan APIs directly.** No processor, utility, codec wrapper, or any other code may call `vulkanalia::Device`, `vkAllocateMemory`, `vkCreateImage`, or any Vulkan function without going through the RHI. This is non-negotiable. (`ash` is fully removed from the workspace per #252; never reintroduce it. CI check #555 enforces.)

The RHI is the **single gateway** to all GPU operations on Linux. Like Unreal Engine's RHI, it gives the runtime absolute control and traceability over every GPU resource.

#### The boundary:
- **`vulkan/rhi/`** (VulkanDevice, VulkanTexture, VulkanPixelBuffer, VulkanVideoEncoder, etc.) — MAY call Vulkan APIs. All GPU memory allocation goes through VulkanDevice via `vulkanalia-vma`.
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

**Exception:** Platform display processors (`linux/processors/display.rs`) may access the underlying Vulkan device handle from GpuContext for swapchain and rendering pipeline setup (this is platform-specific rendering, like Metal rendering on macOS). But they MUST acquire all textures and buffers through GpuContext pools, never allocate GPU memory directly.

#### Compute kernels — single canonical abstraction

All GPU compute work goes through `VulkanComputeKernel` plus the public
`ComputeKernelDescriptor` / `ComputeBindingSpec` types in `core::rhi`.
**Never hand-roll a descriptor set, descriptor pool, command buffer,
fence, or pipeline layout for a compute shader.** Add new kernels by
declaring their bindings as data and calling `GpuContext::create_compute_kernel`.
SPIR-V reflection (via `rspirv-reflect`) validates the declared layout
against the shader at construction. See @docs/architecture/compute-kernel.md
for the full recipe.


### Custom Commands

- `/refine-name <current_name>` - Get MORE explicit naming suggestions (never shorter)

---

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

Index: @docs/learnings/README.md
