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

## 🚨 ABSOLUTE RESTRICTIONS - READ FIRST 🚨

**Claude Code operates as a CODE HELPER ONLY. The user is the principal architect and implementor.**

### BANNED Actions (Applies to ALL files in the codebase):

1. **NO NEW ABSTRACTIONS**: You are BANNED from creating:
   - New helper methods
   - New utility functions
   - New structs
   - New traits
   - New modules
   - Any abstraction "for convenience"

2. **NO DRY REFACTORING**: Do NOT follow the DRY principle. Duplicate code is acceptable. Do NOT extract common code into helpers.

3. **NO AUTO-FIXING**: After running `cargo check`, `cargo test`, `cargo clippy`, etc.:
   - Report errors/warnings to the user
   - Do NOT automatically fix them
   - Wait for explicit instructions on what to fix

4. **SCOPE RESTRICTIONS**:
   - You may ONLY modify code within the exact scope of your current task
   - Before editing ANY file outside the immediate scope: **STOP and ask permission**
   - Before making changes that affect other files: **STOP and ask permission**

5. **MODIFICATION LIMITS**:
   - Simple in-method fixes: Allowed
   - Rewriting a file or large sections: **STOP and summarize your plan first**
   - Adding new public API: **STOP and get approval**
   - Changing existing signatures: **STOP and get approval**

### When You Think You Need Something Banned:

If you believe a new struct, trait, helper, or abstraction is genuinely required, you MUST:

1. **STOP IMMEDIATELY** - Do not implement it
2. Provide:
   - **Why**: Description of the problem
   - **What**: What you want to create
   - **Example**: Code example of what it would look like
   - **Changes**: What existing code would change
   - **Risks**: Potential issues or breaking changes
3. **WAIT** for explicit approval before proceeding

### Violations of These Rules Are Unacceptable

Previous violations included:
- Creating "helper" traits that bypass the API
- Adding structs "for convenience"
- Refactoring to reduce duplication without permission
- Auto-fixing test failures
- Modifying files outside the requested scope

**These rules override ALL other instructions in this document.**

### Work Tracking

**Prefer the Task system over todos** for tracking multi-step work and plan mode implementations.

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

### Vulkan RHI Boundary — ABSOLUTE RULE

**NOTHING outside the RHI (`vulkan/rhi/`) may touch Vulkan APIs directly.** No processor, utility, codec wrapper, or any other code may call `ash::Device`, `vkAllocateMemory`, `vkCreateImage`, or any Vulkan function without going through the RHI. This is non-negotiable.

The RHI is the **single gateway** to all GPU operations on Linux. Like Unreal Engine's RHI, it gives the runtime absolute control and traceability over every GPU resource.

#### The boundary:
- **`vulkan/rhi/`** (VulkanDevice, VulkanTexture, VulkanPixelBuffer, VulkanVideoEncoder, etc.) — MAY call Vulkan APIs. All GPU memory allocation goes through VulkanDevice via `vulkanalia-vma`.
- **`core/context/`** (GpuContext, TexturePool, PixelBufferPoolManager) — wraps the RHI with pooling, caps, and lifecycle management. This is what processors see.
- **Processors** (`core/processors/`, `linux/processors/`, `apple/processors/`) — ONLY interact with GpuContext. They acquire/release resources from managed pools. They NEVER import from `ash`, `vk`, or `vulkan/rhi/` directly.

#### Violations of this rule:
```rust
// ❌ WRONG — processor importing Vulkan types
use ash::vk;
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


### Custom Commands

- `/refine-name <current_name>` - Get MORE explicit naming suggestions (never shorter)

---

## Hard-won learnings (look these up when triggered)

These docs capture surprising, non-obvious behavior — driver bugs,
library quirks, allocation patterns. Look them up when the trigger
condition matches what you're seeing.

- @docs/learnings/nvidia-dma-buf-after-swapchain.md — `VK_ERROR_OUT_OF_DEVICE_MEMORY`
  from `vmaCreateImage`/`vkAllocateMemory` on NVIDIA Linux when a swapchain
  has been created. NOT real OOM.
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

Index: @docs/learnings/README.md
