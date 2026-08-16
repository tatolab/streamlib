# shaderc accepts an entry-point name for GLSL and then ignores it

**Symptom.** `shaderc_compile_into_spv` takes an `entry_point_name`
parameter. Pass `"sharpen"` for a GLSL source whose function is `void main()`
and the call **succeeds**, returning a module whose `OpEntryPoint` still says
`main`. Nothing warns. Building a `VkPipeline` against `"sharpen"` then fails
at pipeline creation, far from the call that caused it — or, worse, silently
runs `main` if some other layer normalizes the name back.

Observed with shaderc 0.10.1 (vendored glslang), targeting Vulkan 1.2 /
SPIR-V 1.4:

```
entry_point="sharpen", source declares main  -> Ok, module contains "main", not "sharpen"
entry_point="sharpen", source declares only sharpen()
                                            -> Err: "Linking compute stage:
                                               Missing entry point: Each stage
                                               requires one entry point"
```

**Root cause.** GLSL has no concept of a selectable entry point — the entry
point is `main`, always. The parameter exists for HLSL, where naming one is
normal. `glslc` exposes the rename as two flags (`--source-entrypoint` to say
which function in the source is the entry, `-fentry-point` to name it in the
output); the library's single `entry_point_name` argument only sets the latter,
so for GLSL it asks glslang to find a function it will never find, and glslang
falls back to `main` rather than erroring. The shaderc-rs safe wrapper exposes
no equivalent of `--source-entrypoint`.

**Fix.** Refuse a non-`main` entry point on GLSL source at construction, and
say why. Do not pass it through hoping the compiler enforces it — the failure
surfaces at pipeline build, naming a Vulkan error rather than the authoring
mistake. The entry point stays meaningful for pre-compiled SPIR-V, where the
blob really can declare another name, and it stays in the compilation cache key
either way.

**Constraint, not a version.** This is what GLSL is, not a gap someone will
close: no shaderc or glslang release will make `void main()` answer to another
name. A pure-Rust front end would behave the same way for the same reason.
