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

### Mandatory Change Approval Workflow

**Applies to code files only:** `.rs`, `.ts`, `.tsx`, `.js`, `.jsx`, `.py`, `.toml`, `.json`, `.yaml`, `.yml`

**Does NOT apply to:** `.md`, `.txt`, documentation, or other text resources

Before making ANY code file modifications, Claude MUST:

1. **Present a Change Manifest** - List ALL files and specific changes planned
2. **Wait for explicit "approved" or "yes"** - Do not proceed without signoff
3. **For any deviation** - Stop immediately, present the new change, wait for approval

Format:
```
CHANGE MANIFEST:
- [file path]: [specific change description]
- [file path]: [specific change description]

Awaiting approval before proceeding.
```

NO exceptions. NO auto-fixing. NO "while I'm here" changes.

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
6. ❌ Modifying source code to make tests work (tests must adapt to the API, not vice versa)
7. ❌ Adding `#[cfg(test)]` to any source file (only the user may add test-only code to source)

**Instead**: Stop, explain the problem, present options, and wait for guidance.

**For testing issues**: When you encounter a situation where the existing API doesn't support what you need to test, STOP and ask the user. Provide:
1. What you're trying to test
2. What the current API requires
3. Why this is a problem
4. Potential options (without implementing them)

### Test Philosophy - CRITICAL

**Tests are a GATING FUNCTION, not a goal.**

The purpose of running tests is NOT to "get them passing." The API is actively evolving, and tests serve to:
1. **Identify cracks** - Where does the current API fall short?
2. **Surface missing pieces** - What's not implemented yet?
3. **Validate design decisions** - Does the API feel right when used?

**When tests fail:**
1. **DO NOT** automatically fix the test or the code
2. **DO NOT** add workarounds to make tests pass
3. **DO** report the failure clearly
4. **DO** analyze what the failure reveals about the API
5. **DO** think carefully about the implications
6. **DO** present options and wait for direction

**The correct response to a failing test is analysis, not action.**

Ask: "What is this failure telling us about the design?" - not "How do I make this pass?"

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

## Project Overview

StreamLib is a real-time audio/video processing framework for Rust, featuring:
- GPU-accelerated video processing (Metal on macOS/iOS, Vulkan planned for Linux)
- Real-time audio processing with CLAP plugin support
- Graph-based processor pipeline architecture
- Platform-specific optimizations (macOS/iOS via Apple frameworks)

## Repository Structure

This is a **Cargo workspace** managing multiple related projects:

```
streamlib/
├── libs/                     # Library crates
│   ├── streamlib/           # Core streaming library
│   │   └── CLAUDE.md        # 📖 Detailed library documentation
│   ├── streamlib-macros/    # Procedural macros for #[streamlib::processor()]
│   │   └── CLAUDE.md        # 📖 Macro implementation details
│   └── yuv/                 # SIMD-optimized YUV/RGB conversion
├── examples/                 # Standalone example applications
│   ├── camera-display/      # Rust: Camera → Display pipeline
│   ├── microphone-reverb-speaker/  # Rust: Audio with CLAP plugins
│   ├── camera-audio-recorder/      # Rust: Record MP4 files
│   └── news-cast/                  # Rust: Multi-source composition
├── docs/                     # Project documentation
└── Cargo.toml               # Workspace configuration
```

### Core Projects

#### `libs/streamlib` - Core Library
The main streaming library implementing the graph-based processor pipeline.
- **Documentation**: See [`libs/streamlib/CLAUDE.md`](libs/streamlib/CLAUDE.md)
- **Purpose**: Core runtime, processor traits, built-in processors, GPU context, and platform integrations
- **Build**: `cargo build -p streamlib`
- **Test**: `cargo test -p streamlib`

#### `libs/streamlib-macros` - Procedural Macros
Provides the `#[streamlib::processor()]` attribute macro for ergonomic processor creation.
- **Documentation**: See [`libs/streamlib-macros/CLAUDE.md`](libs/streamlib-macros/CLAUDE.md)
- **Purpose**: Code generation for processor boilerplate, port introspection, and trait implementations
- **Build**: `cargo build -p streamlib-macros`

#### `libs/yuv` - Color Conversion
SIMD-optimized color space conversions (RGBA ↔ YUV formats).
- **Purpose**: High-performance color conversions for video encoding/decoding
- **Build**: `cargo build -p yuv`

### Examples

Examples are **standalone applications** demonstrating StreamLib usage:
- **Location**: `examples/` directory
- **Build from workspace root**: `cargo build -p camera-display`
- **Run from workspace root**: `cargo run -p camera-display`

Examples also serve as **integration tests** - they must compile and run successfully.

## Development Setup

### Git Hooks (Lefthook)

The project uses [lefthook](https://github.com/evilmartians/lefthook) for automated code quality checks:

**Installation**:
```bash
# Install lefthook (if not already installed)
brew install lefthook  # macOS
# or: cargo install lefthook

# Install git hooks
lefthook install
```

**Hooks**:
- **pre-commit**:
  - `cargo fmt --check` - Verify formatting
  - `cargo check` - Fast compilation check
- **pre-push** (runs sequentially):
  1. `cargo check` - Ensure code compiles
  2. `cargo clippy` - Linting with strict warnings
  3. `cargo test --lib` - Run library tests
  4. Example builds - Verify examples compile

**Skip hooks** (when needed):
```bash
LEFTHOOK=0 git commit  # Skip pre-commit
LEFTHOOK=0 git push    # Skip pre-push
```

Configuration: `.lefthook.yml`

### Broker Service (macOS) - Dev Environment

StreamLib requires the broker service for cross-process GPU resource sharing. The runtime will fail to start if the broker is not running.

**First-time setup (developers)**:
```bash
# Creates local dev environment
./scripts/dev-setup.sh

# This script:
# 1. Creates ./.streamlib/ directory in project root
# 2. Creates proxy scripts that use `cargo run` (auto-rebuild on changes)
# 3. Updates .cargo/config.toml with dev environment variables
# 4. Starts broker on port 50052 with unique service name
# 5. Each git worktree gets isolated broker (based on path hash)
```

After running `dev-setup.sh`, these environment variables are set in `.cargo/config.toml`:
- `STREAMLIB_HOME` → `./.streamlib/` (project-local, not `~/`)
- `STREAMLIB_BROKER_PORT` → `50052` (dev port, not production 50051)
- `STREAMLIB_DEV_MODE` → `1` (enables wheel building, source detection)

This means `cargo run` works directly without manual env var setup.

**No shell config needed** - dev environment is project-local.

**Managing the broker**:
```bash
# Check broker status
./.streamlib/bin/streamlib broker status

# View registered runtimes
./.streamlib/bin/streamlib broker runtimes

# View active connections
./.streamlib/bin/streamlib broker connections
```

**After code changes**: No action needed - proxy scripts use `cargo run`, so changes are picked up automatically on next invocation.

**Clean reinstall** (if needed):
```bash
./scripts/dev-setup.sh --clean
```

**Troubleshooting**:
```bash
# View broker logs (path includes unique hash)
tail -f /tmp/streamlib-broker-dev-*.log

# Check launchd service status
launchctl list | grep Streamlib-dev
```

#### Multiple Worktrees Support

Each git worktree gets its own isolated dev environment:
- Unique service name: `Streamlib-dev-<6-char-hash>`
- Unique broker port: `50052` (same port, but separate launchd service)
- Separate `.streamlib/` directory per worktree
- No conflicts between worktrees

## Quick Start Commands

### Building
```bash
# Build entire workspace
cargo build

# Build library only (faster)
cargo build --lib -p streamlib

# Build specific example
cargo build -p camera-display

# Build with features
cargo build -p streamlib --features debug-overlay
```

### Testing
```bash
# Run all workspace tests
cargo test

# Test specific crate
cargo test -p streamlib
cargo test -p yuv

# Run specific test
cargo test test_name

# Run with output
cargo test -- --nocapture
```

### Running Examples

After running `./scripts/dev-setup.sh`, the dev environment variables are configured in `.cargo/config.toml`, so `cargo run` works directly:

```bash
# ✅ Works - .cargo/config.toml has STREAMLIB_* env vars
cargo run -p camera-display
cargo run -p camera-python-display

# With logging
RUST_LOG=debug cargo run -p camera-display

# Using CLI wrapper (also works)
./.streamlib/bin/streamlib run
```

**Note**: If you haven't run `dev-setup.sh`, examples using StreamRuntime will fail to connect to the broker.

### Documentation
```bash
# Generate and open all docs
cargo doc --open

# Document specific crate
cargo doc -p streamlib --open --no-deps
cargo doc -p yuv --open --no-deps
```

## Architecture Overview

StreamLib uses a **graph-based processing pipeline** where processors are nodes in a directed acyclic graph (DAG):

```
[CameraProcessor] --VideoFrame--> [DisplayProcessor]
                                      ↓
                                  (renders to window)
```

### Key Concepts

- **Processor**: Node in graph implementing `Processor` trait
- **Port**: Typed input/output endpoints (`StreamInput<T>`, `StreamOutput<T>`)
- **Frame**: Data flowing between processors:
  - `VideoFrame` - GPU texture with metadata
  - `AudioFrame<N>` - Audio buffer with N channels (generic const)
  - `DataFrame` - Generic binary data
- **Runtime**: Manages processor lifecycle, threading, scheduling, and GPU context

### Critical Design Patterns

These patterns are fundamental to working with StreamLib. For detailed implementation guidance, see the respective crate documentation.
"
#### 1. Runtime Thread Dispatch (macOS/iOS)
Apple frameworks (AVFoundation, VideoToolbox, CoreMedia) **require** runtime thread execution.

**Solution**: Use `RuntimeContext::run_on_runtime_thread_blocking()` or `run_on_runtime_thread_async()`

See [`libs/streamlib/CLAUDE.md`](libs/streamlib/CLAUDE.md) for details.

#### 2. Processor Macro System
Use `#[streamlib::processor()]` attribute macro to automatically generate boilerplate:

```rust
#[streamlib::processor(
    execution = Reactive,
    description = "My processor description"
)]
pub struct MyProcessor {
    #[streamlib::input(description = "Video input")]
    input: LinkInput<VideoFrame>,
    
    #[streamlib::output(description = "Video output")]  
    output: LinkOutput<VideoFrame>,
    
    #[streamlib::config]
    config: MyConfig,
}
```

See [`libs/streamlib-macros/CLAUDE.md`](libs/streamlib-macros/CLAUDE.md) for implementation details.

#### 3. Monotonic Timestamp System
All frames use monotonic nanoseconds (`i64`) from `MediaClock::now()` - never `SystemTime::now()`.

See [`libs/streamlib/CLAUDE.md`](libs/streamlib/CLAUDE.md#4-timestamp-system-critical-for-av-sync) for timestamp handling.

#### 4. Lock-Free Bus Architecture
Processors communicate via lock-free ring buffers (`OwnedProducer`/`OwnedConsumer`).

See [`libs/streamlib/CLAUDE.md`](libs/streamlib/CLAUDE.md#4-lock-free-bus-architecture-phase-2) for details.

## Development Workflow

### Working on Core Library
```bash
# Navigate to library directory
cd libs/streamlib

# Build, test, document from library directory
cargo build --lib
cargo test
cargo doc --open --no-deps

# Or from workspace root
cd ../../
cargo build -p streamlib
cargo test -p streamlib
```

See [`libs/streamlib/CLAUDE.md`](libs/streamlib/CLAUDE.md) for detailed library development instructions.

### Working on Macros
```bash
# Navigate to macros directory
cd libs/streamlib-macros

# Test macro expansion
cargo expand --test macro_tests

# Or from workspace root
cd ../../
cargo build -p streamlib-macros
cargo test -p streamlib-macros
```

See [`libs/streamlib-macros/CLAUDE.md`](libs/streamlib-macros/CLAUDE.md) for macro development details.

### Adding Examples
1. Create new directory in `examples/`
2. Add `Cargo.toml` with `streamlib` dependency
3. Implement in `src/main.rs`
4. Run from **workspace root**: `cargo run -p example-name`

Examples serve as integration tests and usage documentation.

## Project Conventions

### Dependency Management
**IMPORTANT**: Before suggesting any dependency versions:
1. Check [crates.io](https://crates.io) to find the latest stable version
2. Verify compatibility with project's Rust version and existing dependencies
3. Use the latest compatible version, not outdated versions

Example:
```bash
# Check latest version on crates.io
# For petgraph: https://crates.io/crates/petgraph shows 0.8.3 (not 0.6)
```

### Commit Messages
Use conventional commits with Claude Code attribution:
```
feat: Add WebRTC H.264 encoder processor

Implement VideoToolbox-based H.264 encoding for WebRTC streaming.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

Co-Authored-By: Claude <noreply@anthropic.com>
```

### Git Workflow
- **Main branch**: `main`
- **Feature branches**: `phase-N-feature-name` (e.g., `phase-1-videotoolbox-h264-encoder`)
- **Commit format**: Conventional commits (feat, fix, refactor, docs, etc.)

### Error Handling
- Use `StreamError` enum from `streamlib::core::error`
- Return `Result<T>` from all fallible operations
- Prefer `?` operator over `.unwrap()` in library code
- `.unwrap()` acceptable in examples and tests

### Code Organization
- **Platform-agnostic code**: `libs/streamlib/src/core/`
- **macOS/iOS code**: `libs/streamlib/src/apple/`
- **Platform re-exports**: `libs/streamlib/src/lib.rs` with `#[cfg(target_os = "...")]`
- **DO NOT** use `#[cfg]` inside platform-specific directories (already conditionally compiled)

## Documentation

### Crate-Specific Documentation
- **Core library**: [`libs/streamlib/CLAUDE.md`](libs/streamlib/CLAUDE.md)
  - Detailed architecture, lifecycle, threading, GPU context
  - Apple-specific patterns (main thread dispatch, Metal interop)
  - Adding processors, working with VideoToolbox
  - Performance optimization, testing strategies
- **Procedural macros**: [`libs/streamlib-macros/CLAUDE.md`](libs/streamlib-macros/CLAUDE.md)
  - Macro implementation details, code generation
  - Adding attributes, testing macro changes
  - Arc-wrapped output enforcement

### Rust API Documentation
```bash
# Generate and browse API docs
cargo doc --open
```

## Common Issues

### Circular Dependencies in Examples
Build the library first, then examples:
```bash
cargo build --lib
cargo build -p example-name
```

### Runtime Thread Deadlock
**NEVER** call `run_on_runtime_thread_blocking()` from the runtime thread when the runtime thread is blocked - it will deadlock.

### Platform-Specific Builds
Some processors only compile on specific platforms:
- `CameraProcessor`, `DisplayProcessor` - macOS/iOS only
- `AudioOutputProcessor`, `AudioCaptureProcessor` - macOS/iOS only
- `MP4WriterProcessor` - macOS/iOS only

Use `#[cfg(target_os = "macos")]` in examples that depend on platform-specific processors.

## Tool Preferences

### Rust Analyzer Plugin - USE THIS

This project uses the **rust-analyzer-lsp** Claude Code plugin (`rust-analyzer-lsp@claude-plugins-official`) for semantic code understanding.

**Why use rust-analyzer over grep**:
- **Semantic understanding**: Knows types, traits, and relationships
- **Accurate navigation**: Handles macros, generics, and complex imports
- **Cross-file context**: Understands the entire crate graph

**When to use rust-analyzer**:
- Understanding type definitions and signatures
- Finding where symbols are defined
- Finding all usages before refactoring
- Exploring symbol visibility and scoping

**When grep is still fine**:
- Searching for string literals
- Finding TODO/FIXME comments
- Pattern matching across non-Rust files

### Custom Commands

- `/refine-name <current_name>` - Get MORE explicit naming suggestions (never shorter)

## Additional Resources

- **Cargo workspace**: Manages dependencies and builds across crates
- **Platform support**: macOS (primary), iOS (partial), Linux/Windows (core only)
