# Change: kernel-kind-parity-bar

**From the owner ruling, 2026-08-17**, taken during `/implement #1777` when PR #1896 came to
land four loudly-named graphics refusals against a plan entry that promises none. Narrows
§Graphics' parity claim from *every GPU capability* to every kernel **kind**, and writes the
four gaps into the plan as named dispositions. The rule they discharge is the owner's,
2026-08-03 on #1708: a capability present in Rust authoring but absent in Python gets an owner-visible
disposition — implemented, stubbed-with-ticket, or named-native-only — and "deferred with no
owner" is the failure mode. It has no repo home, so it is restated rather than cited; landing
it as a rule file is `/propose-rule`'s job.

Scale tier: change artifact **plus an ADR edit**, and not a new ADR.
`propose-change/SKILL.md:16-17` fires the "+ ADR" arm on "anything touching the RHI, the IPC wire
format, the processor model, or the Python API's public contract", and the last of those fires
here: no Python signature moves — every refusal below already ships on this branch — but what the
Python API publicly *promises* does. The arm is discharged by a dated annotation to the ADR that
already owns the decision, `docs/decisions/python-kernel-api.md` decision 1: the chosen shape is
unchanged and only its scope narrows, `docs/decisions/README.md:16-18` prescribes that
supersede-in-place form, and a companion ADR would restate decision 1 in a second file to move one
clause of it.

Recon verified at HEAD `648478f7` on 2026-08-17. Precondition satisfied, per entry: the parity
entry (ARCHITECTURE.md:195-198) carries `**DECIDED**`, as does the always-present-capabilities
entry (ARCHITECTURE.md:212-215), named only so the fold does not touch it. §Graphics' trailing
`- **OPEN** — Everything else.` (ARCHITECTURE.md:232) is a catch-all predating the align that did
not stop `python-kernel-surface` being proposed against this section; this change enumerates
inside it and builds nothing against it.

## Behavior after this change

The parity bar is **every kernel kind** — already §Graphics' vocabulary (ARCHITECTURE.md:190,
:221) and GLOSSARY's kind-shaped **Kernel** (GLOSSARY.md:89-91). The plan stops promising Python
can express every pipeline Rust can, and the four capabilities it cannot are spelled into the
plan below. Two sentences must not move with it, because "no kernel capability" appears twice in
§Graphics with two meanings: ARCHITECTURE.md:214-215 is about the deleted bridge traits and
*runtime* absence, not language parity, and ARCHITECTURE.md:209-210 — "a Python compute kernel
reads one surface and writes another, at parity with Rust" — is a shipped claim that stays true.
A find-and-replace narrowing would gut the first; neither is amended.

## The four gaps, at HEAD

Not symmetric, which is why the dispositions land in two places: two are Python-reach gaps
against a Rust capability that exists, two are unbuilt engine capabilities refused in *every*
language; two carry open tickets on a non-MVP milestone, two have none.

**1. Vertex and index buffers, and indexed draws — Rust-only, unexercised, unowned.** Rust:
`acquire_vertex_buffer` (`gpu_context.rs:1846`), `acquire_index_buffer` (`:1861`),
`set_vertex_buffer` (`vulkan_graphics_kernel.rs:1361`), `set_index_buffer` (`:1374`),
`record_draw_indexed` (`vulkan_command_recorder.rs:1166`) — zero callers anywhere, both acquire
symbols occurring only in `gpu_context.rs` (definitions plus its `GpuContextLimitedAccess` and
`GpuContextFullAccess` mirrors), and all three vertex-path methods have zero exercise in the tree;
`constructs_kernel_with_vertex_input_buffers` (`vulkan_graphics_kernel.rs:2837`) sets only
`pipeline_state.vertex_input`. Python gets `gl_VertexIndex` — `draw` takes `vertex_count`,
`instance_count`, `first_vertex`, `first_instance` and no buffer argument (`_engine.pyi:711-721`,
`:474-478`) — and the wire refuses independently at `subprocess_escalate.rs:2492`, `:2504` and
`:3512`. Missing primitive: no escalate op mints a buffer, `acquire_pixel_buffer` lands on
`HostVulkanBuffer::new` with usage `TRANSFER_SRC | TRANSFER_DST | STORAGE_BUFFER`
(`vulkan_buffer.rs:151-154`) and no `VERTEX_BUFFER` bit, while the setters take
`&impl VulkanVertexBindable`, which `PixelBuffer` does not implement. Only the *wire* half ships
elsewhere: `build_triangles_blas` hex-encodes flat Python lists
(`python_processor_context.rs:1068-1069`), decoded and minted engine-side
(`subprocess_escalate.rs:2791-2798`, `vulkan_acceleration_structure.rs:159`) as
`AsBuffer::new_host_visible` — vertex `:201-207`, index `:223-229`, usage
`ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR | SHADER_DEVICE_ADDRESS`, host-visible by
deliberate choice (`:196-200`), no `VulkanVertexBindable` impl. #505 and #658 cover neither.

**2. Depth attachments — unbuilt in every language, with a residual construction asymmetry.**
`DepthStencilState` (`core/rhi/graphics_kernel.rs:416`) and `AttachmentFormats.depth` (`:544-546`)
let Rust *construct* a depth-testing pipeline, exercised only by the hardware-gated
`constructs_kernel_with_depth_stencil_enabled` (`vulkan_graphics_kernel.rs:2763`); Python cannot
name depth at all (`tests/test_graphics_kernel.py:67-89`, `:91-102`; wire refusals
`subprocess_escalate.rs:2511`, `:3491`). That construction surface *is* a real asymmetry, and it
buys nothing: both dynamic-rendering sites build `RenderingInfo` with colour attachments only
(`vulkan_graphics_kernel.rs:684-690`, signature `:560-565` taking `color_targets` and no depth
parameter; `vulkan_command_recorder.rs:880-890`), and consumer-rhi's `TextureFormat`
(`formats.rs:14-29`) has no depth variant to allocate. Owned, non-MVP: #664 and #665, OPEN on
`Graphics Kernel Buildout`.

**3. storage_buffer and uniform_buffer bindings — Rust-only, with live Rust callers.**
`acquire_storage_buffer` (`gpu_context.rs:1803`), `acquire_uniform_buffer` (`:1828`);
`set_storage_buffer` on all three kernel kinds (`vulkan_compute_kernel.rs:354`,
`vulkan_graphics_kernel.rs:371`, `vulkan_ray_tracing_kernel.rs:417`) and `set_uniform_buffer`
beside each (`:377`, `:396`, `:436`). The only one of the four with live non-test callers —
`camera_source.rs:625`, `sdk/vulkan-jpeg/src/kernel.rs:150-151`,
`sdk/vulkan-jpeg/src/vulkan_compute_backend.rs:92-93`; the `surface_export_staging.rs` pair
(`:1115`, `:1153`) sits inside `#[cfg(all(test, target_os = "linux"))] mod tests` opened at `:1032`
and is not a consumer, and `set_uniform_buffer` has no caller anywhere. Python gets a refusal by
name: both kinds are legal wire words and reflection finds them, but dispatch refuses, naming
`storage_image` and `sampled_texture` as the surface-backed kinds
(`subprocess_escalate.rs:1987-1993` for graphics and ray tracing, `:1630-1637` for compute).
Missing primitive: `resolve_texture_registration_by_surface_id` (`gpu_context.rs:1290`) is the
only by-surface-id resolution the escalate path has and it is texture-shaped, so nothing
publishes a buffer to resolve — not a usage-flag problem, since the pixel-buffer allocation
already carries `STORAGE_BUFFER` (`vulkan_buffer.rs:151-154`). #964 and #503 are adjacent and
cover neither. The one gap with a plausible MVP consumer, and the one the owner ruled on.

**4. MSAA — unbuilt in every language, and symmetric.** `samples != 1` is refused for every
caller in any language in the free function `create_graphics_pipeline_with_cache` (declared
`vulkan_graphics_kernel.rs:1967`, check `:1984-1988`) — not in `VulkanGraphicsKernel::new`
(`:1249`), which reaches it only by delegation through `VulkanGraphicsKernelInner::new` — and
pipeline creation then ignores the field, hardcoding
`.rasterization_samples(vk::SampleCountFlags::_1)` (`:2083-2086`). `MultisampleState`'s doc says
the same (`core/rhi/graphics_kernel.rs:388-389`). Python gets a pin: the wire sets
`multisample_samples` to 1 (`python_processor_context.rs:2031`), the host refuses anything else
(`subprocess_escalate.rs:3471-3476`), and the kwarg does not exist
(`tests/test_graphics_kernel.py:91-102`). Owned, non-MVP: #660, OPEN on `Graphics Kernel
Buildout`, whose body records "To the best of our current knowledge MSAA is not on the critical
path for any current consumer; the v1 surface deliberately rejects sample count > 1".

## MODIFIED

- MODIFIED: §Graphics, the Python-parity DECIDED entry
  ARCHITECTURE.md:195-198. Today, verbatim:

  ```markdown
  - **DECIDED** — Python reaches every GPU capability Rust authoring reaches: compute,
    graphics and ray-tracing kernels, acceleration structures, and CPU readback. Python
    names and drives; the engine allocates, compiles, binds, and dispatches. No kernel
    capability is Rust-only. [python-kernel-api]
  ```

  The exact replacement, paste-ready. The enumeration survives verbatim because it is what the
  ruling narrows *to*, and so does "Python names and drives". Gaps 1 and 3 are named as a clause
  inside the entry — the shape of the Apple-capture gap at ARCHITECTURE.md:246-248, putting the
  absence where the claim is read:

  ```markdown
  - **DECIDED** — Python reaches every kernel kind Rust authoring reaches: compute,
    graphics and ray-tracing kernels, acceleration structures, and CPU readback. Python
    names and drives; the engine allocates, compiles, binds, and dispatches. No kernel
    kind is Rust-only. Pipeline state and buffer resources inside a kind are a narrower
    claim, and the two a Python processor cannot reach are named rather than left silent:
    vertex and index buffers with indexed draws — no escalate op mints either buffer, and
    no consumer in either language binds one; and storage- and uniform-buffer bindings —
    Rust consumers in the engine tree hold them, and the only by-surface-id resolution the
    escalate path has is texture-shaped, so a Python processor is refused by name. Both
    are undesigned.
    [python-kernel-api; kernel-kind-parity-bar — the parity claim narrowed to kernel kinds]
    <!-- verify: sdk/streamlib-python-wheel/tests/test_graphics_kernel.py::test_a_draw_takes_no_vertex_buffer_no_index_buffer_and_no_depth_target -->
    <!-- verify: sdk/streamlib-python-wheel/tests/test_graphics_kernel.py::test_a_graphics_kernel_carries_no_depth_or_vertex_input_state -->
  ```

  Plan prose is rewritten, never struck — ARCHITECTURE.md holds no strikethrough anywhere — and
  the bracket's semicolon clause is the document's idiom for a partial later amendment
  (ARCHITECTURE.md:77-78, :296-297), applied with the markers at the ship fold.

  **RESOLVED (owner, 2026-08-17)** — the gap does not block MVP on its own terms, and the one case
  it would block is reopened through a texture rather than a buffer. Of the four, only gap 3
  plausibly bit: depth and MSAA are 3D-raster concerns a fullscreen effect has no use for, and
  `gl_VertexIndex` is the idiom for a fullscreen pass, as the engine's own conformance probe shows
  (`tests/graphics_kernel_probes.py:57-64`). Push constants carry a filter's *knobs* — reachable at
  construction and at dispatch (`_engine.pyi:455`, `:720`), floored by `maxPushConstantsSize` at
  128 bytes guaranteed and 256 on NVIDIA, so 32 floats: a colour matrix plus scalars. What they
  cannot carry is a *data table* — a colour LUT, a film curve, convolution weights — and every
  fallback is independently closed today: binding a buffer is refused
  (`subprocess_escalate.rs:1989`); filling an `rgba32_float` texture from numpy is refused because
  an acquired texture is `DEVICE_LOCAL` and `lock()` raises "surface has no host mapping"
  (`python_processor_context.rs:365`); and a host-visible pixel buffer cannot be bound because
  every kernel-binding site resolves with extent `(0, 0)` (`subprocess_escalate.rs:1675`, `:2025`)
  and the pixel-buffer fallback refuses a zero extent (`gpu_context.rs:1454-1459`).

  The ruling: widen #1758, already OPEN on `MVP`, to cover CPU *write* into an acquired texture.
  That reopens the LUT-as-texture route with no buffer work, so gap 3 files no new ticket and the
  disposition text above stands as written — an honest named gap rather than a hole. Gap 1 files
  post-MVP on `Graphics Kernel Buildout` beside #660, #664 and #665. The alternatives were
  accepting the gap outright for MVP, and minting an MVP ticket for a buffer-minting escalate op
  plus a buffer arm on binding resolution, the largest of the three engine changes.

- MODIFIED: §Graphics, the trailing OPEN entry
  ARCHITECTURE.md:232, today the whole line `- **OPEN** — Everything else.` — expanded to name
  gaps 2 and 4, the shape §Networking's OPEN entry uses at ARCHITECTURE.md:286. They land here
  rather than inside the DECIDED entry because neither is a Python-reach gap:

  ```markdown
  - **OPEN** — Everything else, including the two graphics capabilities no language can
    render: depth attachments — Rust constructs a depth-testing pipeline that Python cannot
    name, and no pass in either language renders against one — and MSAA, refused for every
    caller in every language with the pipeline hardcoded to a single sample. Both are
    unbuilt engine capabilities rather than Python-reach gaps; equalising the construction
    surface with no pass to render against would buy nothing.
  ```

- MODIFIED: docs/decisions/python-kernel-api.md decision 1
  `python-kernel-api.md:15-19`. Annotated, not overwritten, per `.claude/rules/docs-policy.md`'s
  supersession form, which `docs/decisions/README.md:16-18` repeats; `:19` forbids tracker
  references here, so the annotation names no issue or PR number. The block is indented three
  spaces to stay inside numbered item 1 — the in-list shape
  `docs/decisions/importable-python-library.md:27-33` uses in its own item 2. The surviving prose
  stays beneath the block, the layout `docs/decisions/single-binary-launch.md` (block :14-25,
  survivor :27-30) and `docs/decisions/media-io-layering.md` (blocks :15-23 and :25-28, survivor
  :30-31) both use:

  ```markdown
     > ~~Parity is the bar. Python reaches every GPU capability Rust authoring reaches~~
     > — Superseded 2026-08-17 by owner ruling. The bar is every kernel *kind*, which is
     > what the enumeration in this same sentence already names. Pipeline state and buffer
     > resources inside a kind are a narrower claim, and the ones Python cannot reach are
     > named in the plan rather than promised here. The rest of (1) stands unnarrowed:
     > Python still names and drives every kind, and being "a proxy to Rust-powered GPU
     > work — not a lesser scripting surface beside it" claims a relationship, not a
     > surface area.
  ```

  That last sentence is why `:17-19` is left standing: the differentiator claim survives because
  Python still drives every kind, and one of the two named gaps has no Rust consumer either. The
  one place the ADR reads (1) capability-shaped is its batching rationale — per-dispatch blocking
  would have "left a Rust-only capability standing, contrary to (1)"
  (`python-kernel-api.md:122-125`) — and batching shipped, so that is closed reasoning.

## ADDED

Two `<!-- verify: -->` markers, both CI-runnable, spelled in the replacement text above;
§Graphics carries none today.

- ADDED: sdk/streamlib-python-wheel/tests/test_graphics_kernel.py::test_a_draw_takes_no_vertex_buffer_no_index_buffer_and_no_depth_target
  `inspect.signature`, no GPU (`test_graphics_kernel.py:67-89`).
- ADDED: sdk/streamlib-python-wheel/tests/test_graphics_kernel.py::test_a_graphics_kernel_carries_no_depth_or_vertex_input_state
  The construction twin, also signature-only (`:91-102`).

Three further assertions make the dispositions provable **on the rig** and never in CI, each being
`requires_gpu`: `test_graphics_kernel.py:251-265`, `:283-294`, and
`test_ray_tracing_kernel.py:200-214` under that file's module-level `pytestmark` (`:36`). No engine
code is commissioned and no diagram edit is owed — `docs/plan/diagrams/` holds only `system.mmd`,
whose lone kernel reference (`:31`) is neither kind- nor capability-scoped.

## REMOVED

None, verified rather than assumed: run against this file,
`.claude/scripts/ship-change-removed-gate.sh` reports that it declares no removal bullets and exits
0 (`:132-138`); `importable-python-library.md` and `mvp-app-experience.md` already carry zero.
Nothing in the tree retires either — the four refusals this change *records* are shipped behaviour,
and the plan sentence and ADR clause it amends live in trees the gate excludes from its content
sweep, `docs/plan/**` and `docs/decisions/**` (`ship-change-removed-gate.sh:49-56`). A removal
bullet aimed at either would search a string the gate cannot see, match nothing, and pass green
forever; retiring plan text is `/ship-change` step 2's job, which is why this is a modification.

## Ticketing note

A plan-and-ADR wording change inside work already ticketed. `/derive-tickets` should mint
**nothing** for the narrowing — the precedent is
`archive/2026-08-08-mcp-served-with-the-node.md:122-128`, where a subtraction inside ticketed work
told the skill to fold rather than file. The narrowed prose and the ADR annotation ride #1777 /
PR #1896. Two tickets come out of the ruling: widening #1758 to cover CPU write into an
acquired texture, and one post-MVP ticket for gap 1 on `Graphics Kernel Buildout`.

## Notes (not tickets)

- **The section flip is done; the fold is per-entry.** §Graphics' header now reads
  `IN-FLIGHT (→ python-kernel-surface, kernel-kind-parity-bar)` (ARCHITECTURE.md:187), the
  comma-list form §Media I/O uses at ARCHITECTURE.md:234. It cannot flip to SHIPPED while
  `python-kernel-surface` is live, so this fold marks only the two entries it names — as
  ARCHITECTURE.md:77-78 carries a per-clause SHIPPED note under an IN-FLIGHT §Packages header.

- **A fifth refusal exists and is not a fifth disposition.** "importing a foreign DMA-BUF is not
  reachable from a Python processor yet" is still live at `python_processor_context.rs:1200`,
  against a plan sentence promising "Cross-process texture import is part of the capability"
  (ARCHITECTURE.md:204-205). It is an open removal bullet of the in-flight
  `python-kernel-surface` change (`python-kernel-surface.md:170`) — unfinished work with an
  owner, not a permanent gap.

- **§Language SDKs & parity is untouched and owes this change nothing.** Its entries
  (ARCHITECTURE.md:290-315) state no capability-parity claim. The sentence this change narrows
  does not carry the word `parity` at all — it had to be found by reading §Graphics — and the one
  place ARCHITECTURE.md uses the word for a claim, :210, is the shipped compute claim that stays
  true. GLOSSARY.md defines neither `parity` nor `capability`.