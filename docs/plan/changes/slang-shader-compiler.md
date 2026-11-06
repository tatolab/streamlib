# slang-shader-compiler

> **Slang is the shader language**, decided by the owner on 2026-11-06 after a spike. One
> compiler builds every kernel the engine runs, from Slang source, for every target the
> engine has today and the ones Tato Studio will need: SPIR-V now; a fragment stage for a
> material graph and Metal or WGSL later. Slang's interfaces and generics are what let a
> chain of shader nodes, or a material graph, compile from typed parts into one kernel.

Today the engine compiles GLSL twice over. At run time `shaderc` 0.10.1, built from
source and linked into the engine, compiles a kernel's GLSL at construction and caches
the SPIR-V (`runtime/streamlib-engine/src/core/rhi/glsl_shader_source_compiler.rs`). At
build time `runtime/streamlib-engine/build.rs` and `runtime/streamlib-vulkan-video/build.rs`
run the machine's `glslc` over the engine's own shaders for `include_bytes!`. Python passes
GLSL text to `create_compute_kernel`, `create_graphics_kernel` and
`create_ray_tracing_kernel`, or pre-compiled SPIR-V.

**After this change:**
- **One shader compiler: Slang**, linked by the engine for run-time kernels and used by the
  build scripts for the engine's own shaders. `shaderc` and `glslc` leave the tree.
- **Slang is the source idiom.** Every engine shader and every example of the kernel API is
  written in Slang. GLSL stays accepted as a second source language through Slang's own GLSL
  front end, so existing GLSL and `GlslPixelEffect` keep compiling. See [NEEDS DECISION 1].
- **The compiler takes a target**, compute SPIR-V today, so a later change can ask it for a
  fragment stage or another backend without a second compiler.
- **Nothing changes after compilation.** Reflection, binding by the shader's own names, the
  push-constant check and the capability refusals read Slang's SPIR-V unchanged.

**Scale gate — this skill plus an ADR.** It replaces a decided source contract and a
vendored C++ dependency. ADR: `docs/decisions/slang-shader-compiler.md`.

**Sequencing.** Independent of `strict-port-families`. `lightweight-node-kinds` phase 2
(shader nodes and fusion) builds on it.

## Spike evidence (2026-11-06, Slang 2026.12)

Measured from Rust through the `shader-slang` 0.1.0 crate, next to the engine's shaderc
settings (Vulkan 1.2, SPIR-V 1.4). The harness lives outside the tree; its numbers are
recorded here so the tickets need not re-derive them.

- **Reflection.** `rspirv-reflect` 0.9 read Slang's output of a fused blend, tint and
  vignette kernel unchanged: the same three set-0 bindings, the same 28-byte std430
  push-constant block and the same workgroup as shaderc's. Binding names survived with debug
  info off.
- **Debug info.** With debug info on, Slang emits `NonSemantic.Shader.DebugInfo`
  instructions that `rspirv` refuses to parse. `reflect_spirv_without_its_debug_source_strings`
  does not strip them.
- **Composition.** A chain written as a generic `Then<Tint, Vignette>` over an `IPixelNode`
  interface compiled to the same SPIR-V as the hand-fused function, 2,848 bytes each.
- **The repo's GLSL.** With GLSL enabled, Slang compiled 27 of the 33 GLSL shaders in the
  engine, test fixtures and consumers with the same reflected bindings and push constants as
  shaderc, all valid under `spirv-val`. Three ray-generation shaders hit a Slang internal
  error on `traceRayEXT`; two shaders calling `texelFetch` on a separate `texture2D` found no
  overload; one needs a define the harness did not pass. The ray-generation and `texelFetch`
  cases compiled once rewritten in Slang syntax.
- **Cost.** `libslang-compiler` is 29.8 MB, and the optimiser `libslang-glslang` 10.0 MB. One
  global session takes 225 to 300 ms to create, once per process. A recompile takes about 40 ms,
  against 4 to 5 ms for shaderc; it runs on a graph edit or kernel construction, never per
  frame, and the cache stays in front of it.
- **macOS.** SPIRV-Cross, which MoltenVK uses, translated Slang's SPIR-V to MSL. The slangpy
  0.43.1 macOS library has a 26.0 minimum, the macOS floor.
- **Licence.** Slang's `LICENSE` at tag `v2026.12` is `Apache-2.0 WITH LLVM-exception`. The
  optimiser carries glslang and SPIRV-Tools, which the wheel already ships through shaderc.

## ADDED

### The compiler

- One module in `runtime/streamlib-engine/src/core/rhi/` owns every Slang call: one global
  session per process, created on first use and kept for the process's life; a session per
  target; load from a source string; the entry point checked by stage; and the SPIR-V out.
  It replaces `glsl_shader_source_compiler.rs` and keeps its role and its cache.
- **The binding is `shader-slang`, pinned by exact version** behind that module. Its three
  missing calls, loading from a source string, `findAndCheckEntryPoint`, and a global session
  with GLSL enabled, go through its raw tables in that module only. Nothing else imports it.
- **Compile settings:** SPIR-V 1.4 for Vulkan 1.2, optimisation on with the optimiser shipped,
  debug info off. Slang keeps binding names without it, and the engine's reflection refuses
  Slang's debug instructions.
- **The cache key** keeps every input that changes the output and names the compiler as
  `slang <version>`, read from the global session's build tag at run time. The source language
  joins the key.
- **The target** is a parameter of the compiler: a stage and a backend. This change serves
  every stage the engine has today (compute, vertex, fragment and the ray-tracing stages) to
  SPIR-V. Another backend is a later change.
- **A source language is declared, never sniffed:** the kernel constructors take
  `source_language`, `"slang"` by default. A compiler diagnostic names the line of the
  source the caller passed, in Slang's own words.

### Packaging

- The wheel's lend and `tatolabd` carry `libslang-compiler` and `libslang-glslang` for each
  floor, beside the engine, as the bundled Vulkan driver is carried. GLSL input also needs
  `libslang-glsl-module`, which ships with them.
- They come from Slang's release archives, pinned by version and checksum and staged by a
  script the build calls. A library whose glibc floor is above `manylinux_2_28`, or whose macOS
  minimum is above 26.0, is refused by the stage script, and that floor builds Slang from
  source in its own build image instead.
- The portability proof extends to them: a C++ runtime and the system only, signed on macOS.
- `THIRD-PARTY-NOTICES.md` names Slang's licence and the projects it carries.

### The engine's own shaders

- The engine's and `streamlib-vulkan-video`'s shaders are rewritten in Slang, one file each,
  and the build scripts compile them through the same library.
- The five that Slang's GLSL front end refuses are among them, so no shader depends on the
  front end's gaps.

## MODIFIED

- **The GLSL source contract** (`ARCHITECTURE.md:2087-2094`, "GLSL is the shader source
  contract"). It reads: Slang is the shader source contract. Python passes Slang text, or GLSL
  through Slang's GLSL front end with `source_language="glsl"`, and the engine compiles it at
  construction with the same cache rule. Pre-compiled SPIR-V stays the escape hatch. The wheel
  carries the Slang compiler, so authoring needs no toolchain beyond the wheel.
- **The wheel's contents** (`ARCHITECTURE.md:3755-3756`, "the wheel carries a C++ GLSL shader
  compiler"). It carries the Slang compiler libraries.
- **`GlslPixelEffect`** (`ARCHITECTURE.md:2236`). It keeps its GLSL body and compiles through
  Slang's GLSL front end. Its future is decided with `lightweight-node-kinds`' `@node.shader`.
- **The capability refusals** (`ARCHITECTURE.md:1934-1937`). A subgroup refusal reads Slang's
  SPIR-V as it reads shaderc's today.
- **`docs/decisions/python-kernel-api.md`** item 4 and its compiler rationale are struck in
  place as superseded by this change.

## REMOVED

- REMOVED: shaderc
- REMOVED: glsl_shader_source_compiler
- REMOVED: VENDORED_GLSL_COMPILER_VERSION
- REMOVED: glslc

## Decisions

- **[NEEDS DECISION 1] Keep GLSL as an accepted source language?** Slang's GLSL front end
  compiled 27 of the repo's 33 GLSL shaders unchanged.
  - *Keep it*, declared with `source_language="glsl"`. Existing user kernels, `GlslPixelEffect`
    and pasted shader code keep working; the front end's gaps (ray-generation `traceRayEXT`,
    `texelFetch` on a separate `texture2D`) refuse with Slang's diagnostic.
  - *Slang only.* One language to document; every consumer kernel ports.
  *Recommendation:* keep it. It costs a 1.3 MB library and one keyword, and the cache and the
  compiler stay one.
- Assumed unless overruled: the entry point stays `main` by default, as today, and any name
  is accepted, since Slang renames entry points where glslang would not.

## Proof

- Every engine kernel test passes with `shaderc` gone from `Cargo.lock`.
- A Slang compute kernel from Python reflects its binding names and dispatches, and a GLSL one
  with `source_language="glsl"` does the same.
- A GLSL ray-generation shader refuses at construction with Slang's diagnostic, never a crash.
- A recompile of a cached source is a cache hit; a compiler version change is a miss.
- The portability proof passes with the Slang libraries in the lend, on both floors.
- `Build (macOS)` is green with the Slang libraries staged and signed.

## Tickets (expected)

1. The Slang compiler module and the packaging, behind the existing constructors: a Slang
   compute kernel from Python, on both floors.
2. The engine's own shaders in Slang through the build scripts; `shaderc` and `glslc` removed.

## Notes (not in scope)

- Consumer kernels in `examples/` and `packages/` move to Slang as each consumer's backlog.
- A fragment target for a material graph, and Metal or WGSL output, are later changes that
  pass a target to this compiler.
- Slang's autodiff is available to any later node that needs gradients.
