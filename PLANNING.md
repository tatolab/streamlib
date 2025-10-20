# streamlib: Building the Infrastructure for Shared Machine Vision

## The Core Insight

**Agents need to share real-time vision and generate video effects as code.**

Not video streaming. Not pre-rendered effects. Not gluing libraries together.

**Real infrastructure** where:
- Multiple entities (robots, helmets, systems) share the same visual reality
- They generate effects/overlays by writing shaders in HLSL
- Everything stays on GPU (zero-copy, sub-10ms latency)
- It works on edge devices (Jetson, embedded Linux, armor)

## Why This Matters

Every company building AR headsets, humanoid robots, or autonomous systems **assumes this infrastructure exists**.

It doesn't.

They're all using:
- Game engines (wrong abstraction - built for entertainment, not real-world systems)
- CV libraries from 2000 (CPU-bound, copies everywhere)
- ML frameworks that don't integrate with rendering

Result: 80ms lag between detection and display. Unacceptable.

## The Vision (What People Will See)

**Not**: "Here's our GPU pipeline architecture"
**Instead**: Magic that makes people ask "how the fuck?"

Examples:
- Put on a helmet. Share vision with your robot teammate in real-time. It highlights threats you missed. You're not controlling it - you're thinking together.
- A swarm of robots. They're simulating possibilities, sharing a dream space, solving problems you never explained. Like watching birds flock. Emergent behavior.
- Self-assembling machines that coordinate through shared visual understanding.

The tech disappears. The experience is what matters.

## Why Rust (Not Python)

**Python was validation.** We proved:
- ✅ Zero-copy camera → GPU → display works
- ✅ Real-time ML inference with GPU textures works
- ✅ Custom shader effects without framework overhead works
- ✅ Agent-controlled effects via decorators works

**Python can't do what we need:**
- Can't access IOSurface/DMA-BUF directly (OS-level zero-copy)
- Can't compile HLSL → SPIR-V → Metal/Vulkan (need DXC, SPIRV-Cross)
- GIL and GC pauses break real-time guarantees
- Doesn't work on embedded systems (Jetson controllers, armor CPUs)

**Rust is required for:**
- Direct Metal/Vulkan API access (no abstractions hiding features)
- Zero-copy texture sharing (IOSurface on macOS, DMA-BUF on Linux)
- Shader compilation pipeline (HLSL → SPIR-V → native)
- Real-time guarantees (no GC, no GIL, sub-millisecond consistent)
- Embedded deployment (cross-compile to ARM, works on Jetson)

## The Architecture

**See [architecture.md](./architecture.md) for complete technical details.**

### What We're Building

```
Python API (stays simple, decorator pattern works)
    ↓
Rust Core (streamlib-core)
  - Direct Metal/Vulkan GPU access
  - HLSL → SPIR-V → Metal/Vulkan compilation
  - Zero-copy texture management (IOSurface/DMA-BUF)
  - Real-time guarantees
    ↓
Hardware (macOS/iOS Metal, Linux Vulkan, Jetson)
```

### Key Architectural Decisions

1. **Runtime as Primitive Provider**
   - Rust core provides low-level primitives (textures, shaders, platform access)
   - NOT hardcoded effects - agents compose primitives in Python
   - Infinitely extensible by design

2. **Native Performance with Embedded Python**
   - Python only for setup and control flow
   - GPU pipeline is 100% Rust (zero Python overhead)
   - Embedded Python interpreter for dynamic agent code execution

3. **Unified Apple Platform Support**
   - Single `streamlib-apple` crate for iOS + macOS
   - Both use Metal, IOSurface, AVFoundation, ARKit
   - iOS gets extra features (body tracking, face mesh)
   - Same agent code runs on both platforms

4. **Multiple Deployment Modes**
   - Library mode: Python wheel for development
   - Binary mode: Standalone daemon for production
   - Remote mode: Network-connected multi-device swarms

### Key Technologies

1. **Agent Shader Protocol**
   - Agents exchange HLSL source code (human-readable)
   - Runtime compiles: HLSL → SPIR-V (via DXC) → Metal/Vulkan (via SPIRV-Cross)
   - Cached compilation (~100ms first time, <1ms thereafter)
   - Self-healing: receiving agent can fix broken shaders

2. **Zero-Copy Pipelines**
   - macOS: IOSurface → Metal texture (no CPU copy)
   - Linux: DMA-BUF → Vulkan texture (no CPU copy)
   - Camera, ML, rendering all on GPU
   - Data never touches CPU unless necessary

3. **Real-Time Guarantees**
   - Rust (no garbage collection pauses)
   - Lock-free where possible
   - Sub-millisecond frame latency
   - 60 FPS @ 4K guaranteed

## What We're NOT Doing

❌ Building a game engine
❌ Building a "product" (this is infrastructure)
❌ Targeting web browsers
❌ Optimizing for ease of implementation (optimize for real-world deployment)
❌ Using abstractions that hide necessary features (wgpu blocked us, we're going native)

## Migration Plan

### Phase 1: Rust Core (Current)
- Native Metal texture interop (IOSurface)
- HLSL → SPIR-V → Metal shader compilation
- Basic PyO3 bindings
- One working example ported from Python

### Phase 2: Feature Parity
- All Python examples work with Rust backend
- Decorator API still works (just calls Rust underneath)
- Linux/Vulkan support (Jetson deployment)
- Agent protocol implementation

### Phase 3: Beyond Python's Limits
- Multi-device coordination (helmet + robot swarm)
- Shared simulation space (agents dream together)
- Self-healing shader compilation
- Embedded deployment (armor controllers)

## Key Principles

1. **Start from the future, work backwards**
   - Don't ask "what libraries exist?"
   - Ask "what should exist?" then build it

2. **Prove it works, then scale**
   - Python proved the concept
   - Rust makes it real
   - Don't skip the proof phase

3. **The demo is the product**
   - Not architecture diagrams
   - Not technical specs
   - Emergent behavior that looks like magic

4. **Ship infrastructure, not applications**
   - We're not building a robot
   - We're building what lets robots share vision
   - Others build on top

## Success Metrics

Not revenue. Not users. Not GitHub stars.

**Success = Someone building the future uses this as their foundation.**

When WorldLabs wants to deploy their world models on hardware.
When Anduril needs shared vision for robot swarms.
When humanoid robots need to coordinate in real-time.

They use streamlib. Because nothing else exists.

## Current Status

- ✅ Python proof-of-concept (validated it's possible)
- 🔨 Rust core (in progress - this is the real thing)
- ⏳ Multi-device demos (waiting for Rust core)
- ⏳ Emergent swarm behavior (the "holy shit" moment)

## Why This Will Work

Because we're not building what's easy. We're building what's necessary.

And we're willing to:
- Read native GPU API documentation
- Fork libraries that block us
- Write in whatever language solves the problem
- Spend time on infrastructure others skip

Everyone else is building on sand (game engines, Python glue).
We're building bedrock.
