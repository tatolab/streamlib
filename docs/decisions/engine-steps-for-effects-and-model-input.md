# Effects and model input as short steps over the engine's own primitives

Rationale for the `[engine-steps-for-effects-and-model-input]` entries in
`docs/plan/ARCHITECTURE.md` §Graphics and §Product, decided by the owner 2026-09-22. The
design memo with code sketches and cited precedent is
`docs/research/2026-09-22-engine-steps-for-effects-and-model-input.md`.

## Trigger

Read this before adding a new way to run a shader from Python, before giving Python another
buffer kind, before adding a pre-processing built-in or node, before changing what
`streamlib new` writes, and when someone asks why a pixel effect or a model's input is not
an engine object of its own.

## The decision

1. **The right thing is the short thing.** People, and especially agents, should reach GPU
   speed by writing only what is theirs: a shader body for an effect, a model's input
   spec for a detector. Every effect tool surveyed (TouchDesigner, Godot, OBS, Unity's
   low-code fullscreen pass, GStreamer's `glshader`) leads with a few-line shader body with
   the plumbing declared for you. Every AI pipeline runtime surveyed (Holoscan, DeepStream,
   MediaPipe) ships model-input pre-processing as a built-in GPU step.
2. **Both steps are wheel grammar over primitives that already exist.** `GlslPixelEffect`
   compiles an ordinary compute kernel around the user's body and uses the texture ring and
   the engine surface copy. `ModelInputTensorKernel` is one compute pass into a tensor
   buffer. Neither is a second kernel system; the engine and the wire gain nothing for
   either. The plan already builds `ProcessorOutputTextureRing` and `VideoFrame` this way.
3. **The tensor buffer is the one engine capability, and it serves two consumers.** Python
   could not hold a storage buffer: no door acquired one, dispatch refused buffer bindings,
   and DLPack export was pixel-shaped. Model input needs one, and so does a learning
   primitive such as a forward-forward layer. One capability — acquire with a tensor shape
   and dtype, bind by id, export over DLPack as that shape, travel by id — answers both.
4. **The scaffold shows the pathway in two files:** a pixel effect on the GPU, and a CPU
   processor that reasons over an explicit, cheap view of the frame. Half of the AI apps
   this runtime is for are logic processors — remote typed inference, a local model
   server — so the second file is as representative as the first.
5. **The scaffold flips only when both floors can run it.** A starter that refuses at
   `setup()` on a Mac breaks the first minute worse than a CPU effect does.

## Rejected alternatives

- **An engine-side pixel-effect object with its own escalate op** — a second way to make a
  compute kernel.
- **A built-in effect node or a built-in pre-processing node** — neither meets the
  built-in criterion, and each costs a helper process and a hop per use; a three-line
  Python processor around the wheel step gives the node shape to anyone who wants it.
- **Pre-processing as a torch recipe in the wheel** — torch-only, its output cannot leave
  the processor, and it would be deleted the day the tensor buffer lands.
- **A float pixel buffer standing in for a tensor** — reinterprets pixel memory as a
  tensor; it works by accident and names nothing.
- **Colour conversion inside either step** — a second concern; YUV is converted by the
  engine before publication.
- **Binding a texture-backed source directly instead of copying** — needs the backing
  named on the Python surface, which no door does; the copy is one GPU-side operation, and
  skipping it is the engine's optimisation to derive later.
- **`vec3` dials** — their push-block alignment silently shifts every dial after them.
- **A GPU-only scaffold, or numpy with a `--gpu` template** — the first shows one leg of
  the pathway; the second leaves the fast path optional.
- **A Rust pixel-effect helper now** — no Rust consumer names one.

## Consequences

- Python gains one surface kind, the tensor buffer; uniform buffers stay undesigned.
- Every per-frame effect pays two synchronous engine round trips (copy, then dispatch).
  If that shows in a measured budget, folding the copy into the dispatch is the engine's
  follow-up.
- The scaffold keeps two dependencies; torch never enters it.
- The scaffold's flip, the effect step and the pre-processing step all ride the Mac
  parity work, which the owner scheduled into the same milestone.
