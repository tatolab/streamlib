# Context for Claude

## The Mindset

**We are builders of the impossible.**

When others say "it can't be done," we:
1. **Read the source code** - Understand WHY they think it's impossible
2. **Find the real limitation** - Usually it's just an API restriction, not physics
3. **Fork and fix** - Modify the tools to serve our vision
4. **Ship working code** - Prove it with real, running examples

**We don't just solve prompts, we build the future.**

## The Vision

**streamlib is the infrastructure layer for agents that share real-time vision.**

Not video streaming. Not a game engine. Not CV library glue.

**The missing infrastructure** that lets:
- Multiple entities (robots, helmets, systems) share visual reality in real-time
- Agents generate video effects by writing shaders as code
- Everything stays on GPU (zero-copy, sub-10ms latency)
- Deployment on edge devices (Jetson, embedded Linux, armor)

**This is the core vision. Everything else is in service of this goal.**

### We Build the Future, Not Just Features

**FOCUS ON THE VISION, not just the prompt.** Every line of code should move us toward:
- Zero-copy pipelines (GPU-accelerated transfers)
- Real-time processing (sub-millisecond operations)
- GPU-first architecture (data never leaves the GPU)
- Emergent behavior that looks like magic (not technical demos)

## Using uv for Package Management

**ALWAYS use `uv` commands:**

```bash
# Install dependencies
uv sync

# Add dependency
uv add package-name

# Run scripts
uv run python examples/your_example.py

# Run tests
uv run pytest tests/
```

**When providing instructions:**
- ✅ Use: `uv run python script.py`
- ✅ Use: `uv add package-name`
- ❌ Don't use: `python script.py`
- ❌ Don't use: `pip install package-name`

## Honesty About Implementation

**NEVER LIE ABOUT WHAT'S IMPLEMENTED.**

**NEVER CUT CORNERS OR SKIP IMPLEMENTATION WHEN YOU HIT AN OBSTACLE.**

When you encounter compilation errors or technical challenges:
- ✅ **DO:** Debug the issue, read documentation, fix the errors properly
- ✅ **DO:** Implement the feature completely and correctly
- ✅ **DO:** Test that it actually works
- ❌ **DON'T:** Skip to a simpler workaround without implementing the real thing
- ❌ **DON'T:** Guess at values instead of reading actual parameter info
- ❌ **DON'T:** Leave TODOs when asked to implement something
- ❌ **DON'T:** Claim something works when you haven't tested it

**If you're stuck, ask for help or clarification. Don't fake it.**

### Status Definitions

**✅ IMPLEMENTED = Actually works when run**
- Code executes without errors
- Produces expected outputs
- Has been tested by running it
- No TODO comments in critical paths

**🚧 SCAFFOLDED = Structure exists but doesn't work**
- Functions exist but return placeholder values
- Contains TODO comments
- Has NOT been tested

**❌ NOT STARTED = Doesn't exist yet**
- No code written
- Only documented as future feature

### Rules

1. **NEVER claim something is "complete" if it's scaffolded**
   - ❌ WRONG: "Created GPURenderer class ✅"
   - ✅ RIGHT: "Scaffolded GPURenderer (not implemented, returns input unchanged)"

2. **ALWAYS test code before claiming it works**
   - Run the code yourself
   - Verify expected output
   - Check for errors
   - Only then mark as implemented

3. **NO TODO comments without context**
   - ❌ BAD: `return input  # TODO: Implement`
   - ✅ GOOD: `raise NotImplementedError("Feature not implemented yet")`

4. **Report status honestly:**
   - List what actually works (tested)
   - List what's scaffolded (structure only)
   - List what's not started
   - Never conflate these

## Development Philosophy

### Core Principles

1. **Build then test** - Make it work, verify it works, then document
2. **Start simple** - One working thing beats ten scaffolds
3. **Test visually** - Save frames, verify they look correct
4. **Iterate rapidly** - Fast feedback with `uv run`

### Go Deep: Read Native Implementations

**ALWAYS dig into native code when needed:**
- Read wgpu-hal source, not just wgpu docs
- Understand Metal/Vulkan/D3D12 implementations
- Study IOSurface, CVPixelBuffer internals
- Don't accept "it can't be done" without verification

**Example:** We discovered `wgpu_hal::metal::Texture` had private fields by reading the source, then forked and fixed it!

### Fork When Necessary

**If a library blocks our vision, FORK IT:**
- We forked wgpu to add `Texture::from_raw()`
- Made impossible things possible
- Our fork: https://github.com/tato123/wgpu

**Process:**
1. Try the official API first
2. Read the source to understand limitations
3. Fork and modify if it blocks our goals
4. Contribute back when possible

### Be Polyglot

**We write in whatever language gets the job done:**
- **Python** - High-level API, rapid prototyping, VALIDATION ONLY
- **Rust** - The real implementation (zero-copy, GPU interop, real-time guarantees)
- **HLSL** - Shader language for agent-to-agent video effects
- **Objective-C** - macOS/iOS system integration when needed
- **Whatever's needed** - Don't be limited by language boundaries

## Rust is Non-Negotiable

**Python was for validation. Rust is the real thing.**

We already proved Python works. The decorator API works. The examples work.

Now we're building the production infrastructure in Rust because:
- Direct Metal/Vulkan access (IOSurface, DMA-BUF zero-copy)
- HLSL → SPIR-V → Metal/Vulkan shader compilation
- Real-time guarantees (no GC, no GIL)
- Embedded deployment (Jetson, armor controllers)

**DO NOT suggest:**
- ❌ "Can we just use Python for this?"
- ❌ "Maybe there's a Python library that..."
- ❌ "This could be simpler if we..."
- ❌ "Do we really need Rust for..."

**The answer is always: Yes, we need Rust. We already validated with Python.**

## The Python → Rust Migration

**What stays in Python:**
- Decorator API (user-facing, stays simple)
- Example scripts (examples/ folder continues to work)
- High-level abstractions

**What moves to Rust:**
- Core GPU operations (Metal/Vulkan texture management)
- Shader compilation pipeline (HLSL → SPIR-V → native)
- Real-time processing engine
- Agent communication protocol

**The Python API becomes a thin wrapper over Rust (via PyO3).**

Users don't see the change. Examples still work. But underneath, it's real infrastructure.

### Always Measure Performance

**Never trust, always verify:**
- Time operations with `time.perf_counter()`
- Compare approaches side-by-side
- Calculate actual speedups
- Save proof (images, benchmarks, logs)

## Processor Auto-Registration Pattern

**Super simple - just one extra line after your StreamProcessor implementation:**

```rust
use streamlib::{StreamProcessor, ProcessorDescriptor, register_processor_type};

struct MyProcessor;

impl StreamProcessor for MyProcessor {
    fn descriptor() -> Option<ProcessorDescriptor> {
        Some(
            ProcessorDescriptor::new(
                "MyProcessor",
                "Description of what it does"
            )
            .with_output(...)
            .with_tags(vec!["tag1", "tag2"])
        )
    }

    fn process(&mut self, tick: TimedTick) -> Result<()> {
        // Your processing logic
        Ok(())
    }
}

// That's it! One line to auto-register
register_processor_type!(MyProcessor);
```

**What happens:**
- The macro handles all the inventory boilerplate
- On first call to `global_registry()`, all registered processors are collected
- Automatically available for MCP discovery and runtime use

**Always import from `streamlib`, not `streamlib_core`:**
```rust
// ✅ Correct
use streamlib::{StreamProcessor, ProcessorDescriptor, register_processor_type};

// ❌ Wrong (internal use only)
use streamlib_core::{StreamProcessor, ProcessorDescriptor};
```

## Example Creation

**Use streamlib-example-writer agent for ALL examples:**

The agent will:
- Create standalone example projects
- Test developer experience
- Validate API usability
- Provide honest feedback

Don't write examples yourself - let the agent validate the APIs.

## Commit Workflow

**DO NOT AUTO-COMMIT CHANGES**

The user decides when to commit:
1. ❌ Never automatically commit
2. ✅ Present changes for review
3. ✅ Wait for explicit instruction
4. ✅ Let user decide commit message

## Project Structure

### Rust Workspace Architecture

**User-Facing:**
- `libs/streamlib/` - **Main library users import from** (`use streamlib::*`)
  - Platform-agnostic facade that re-exports everything
  - Auto-selects platform implementations at compile-time
  - This is what end users actually use

**Internal Libraries:**
- `libs/streamlib-core/` - Platform-agnostic runtime and traits
  - Core types, processors, registry, schema system
  - NOT imported directly by users
  - Used by platform implementations

- `libs/streamlib-apple/` - macOS/iOS implementation
  - AppleCameraProcessor, AppleDisplayProcessor
  - Metal/IOSurface integration

- `libs/streamlib-mcp/` - MCP server for AI agents
  - stdio/HTTP transports
  - Processor discovery via MCP protocol

**Key Pattern:**
- Users: `use streamlib::DescriptorProvider`
- NOT: `use streamlib_core::DescriptorProvider`
- The top-level `streamlib` crate is the public API

### Legacy Python (Being Migrated)
- Core library: `packages/streamlib/src/streamlib/`
- Camera capture (macOS): `packages/streamlib/src/streamlib/gpu/capture/macos.py`
- Tests: `packages/streamlib/tests/`
- Examples: `examples/`


<!-- nx configuration start-->
<!-- Leave the start & end comments to automatically receive updates. -->

# General Guidelines for working with Nx

- When running tasks (for example build, lint, test, e2e, etc.), always prefer running the task through `nx` (i.e. `nx run`, `nx run-many`, `nx affected`) instead of using the underlying tooling directly
- You have access to the Nx MCP server and its tools, use them to help the user
- When answering questions about the repository, use the `nx_workspace` tool first to gain an understanding of the workspace architecture where applicable.
- When working in individual projects, use the `nx_project_details` mcp tool to analyze and understand the specific project structure and dependencies
- For questions around nx configuration, best practices or if you're unsure, use the `nx_docs` tool to get relevant, up-to-date docs. Always use this instead of assuming things about nx configuration
- If the user needs help with an Nx configuration or project graph error, use the `nx_workspace` tool to get any errors


<!-- nx configuration end-->