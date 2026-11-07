# Slang shader compiler

Rationale for the `[slang-shader-compiler]` entries in `docs/plan/ARCHITECTURE.md` §Graphics,
decided by the owner 2026-11-06. Supersedes item 4 of `python-kernel-api.md`, GLSL as the
source contract.

## Trigger

Read this before adding a shader compiler, a shader language, a shader target or a second
kernel cache, and when someone asks why kernels are written in Slang rather than GLSL, HLSL
or WGSL.

## Decision

Slang is the shader source language, and one Slang compiler inside the engine builds every
kernel: user kernels at construction, the engine's own shaders at build time. Slang is the only
source language; GLSL is retired. The compiler takes a target, so a fragment
stage for a material graph, or Metal and WGSL output, is a parameter rather than a second
compiler.

## Why

**Node graphs compile to shaders.** Tato Studio fuses chains of shader nodes into one kernel,
and a material graph is a shader built from parts. Slang's interfaces and generics express
both as types: a chain written as `Then<Tint, Vignette>` over one node interface compiled to
the same SPIR-V as the hand-fused function. GLSL has no modules, interfaces or generics, so
fusing in GLSL means pasting text.

**One source, every backend.** Slang emits SPIR-V, Metal, WGSL, HLSL and CUDA from the same
source. The engine runs SPIR-V on Vulkan everywhere today, through MoltenVK on macOS; the
other targets stay open without a rewrite.

**It fits what the engine already does.** The engine's reflection, binding-by-name and
push-constant checks read Slang's SPIR-V unchanged, and the cache in front of the compiler
stays.

## Rejected alternatives

- **Slang with GLSL still accepted through its GLSL front end** — a second language to
  document, a parameter, a library and a set of front-end gaps, for compatibility nobody needs
  yet (owner, 2026-11-06).
- **Stay on GLSL** — no composition, so shader-node fusion and material graphs become string
  templating.
- **HLSL through DXC** — interfaces but no generic composition of the kind fusion needs, and a
  Windows-first toolchain.
- **WGSL through naga** — a web-first language without generics, and naga's front ends do not
  cover ray-tracing pipeline stages.
- **Slang beside shaderc** — two compilers, two caches and two diagnostic formats for one
  job.

## Consequences

- The wheel and `tatolabd` carry about 40 MB of Slang libraries per floor, and a recompile
  takes about 40 ms where shaderc took 5 ms. Both were accepted for the outcome.
- The Rust binding is a young third-party crate, pinned and confined to one module; if it
  stalls, the owner maintains it.
- Slang's debug info is off: the engine's reflection cannot parse its non-semantic debug
  instructions, and binding names survive without it.
- Every GLSL shader in the engine tree is rewritten in Slang, and `GlslPixelEffect` gives way
  to `@node.shader`; consumers' GLSL ports as backlog.
