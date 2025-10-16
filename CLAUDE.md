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

**streamlib is a realtime streaming platform where AI agents can easily compose:**

- Live camera streams
- ML models (object detection, segmentation, etc.)
- Dynamic audio/video generation
- Real-time visual effects and overlays
- All running on GPU at 60fps

**This is the core vision. Everything else is in service of this goal.**

### We Build the Future, Not Just Features

**FOCUS ON THE VISION, not just the prompt.** Every line of code should move us toward:
- Zero-copy pipelines (GPU-accelerated transfers)
- Real-time processing (sub-millisecond operations)
- GPU-first architecture (data never leaves the GPU)
- Developer experience that makes the complex simple

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
- **Python** - High-level API, rapid prototyping
- **Rust** - Zero-copy operations, GPU interop, performance
- **WGSL/Metal/GLSL** - Shader programming
- **Objective-C** - macOS/iOS system integration
- **Whatever's needed** - Don't be limited by language boundaries

### Always Measure Performance

**Never trust, always verify:**
- Time operations with `time.perf_counter()`
- Compare approaches side-by-side
- Calculate actual speedups
- Save proof (images, benchmarks, logs)

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

- Core library: `packages/streamlib/src/streamlib/`
- Camera capture (macOS): `packages/streamlib/src/streamlib/gpu/capture/macos.py`
- Rust extensions: `packages/streamlib/rust/`
- Tests: `packages/streamlib/tests/`
- Examples: `examples/`
- Documentation: `packages/streamlib/README.md`
- Forked dependencies: Track in pyproject.toml and Cargo.toml
