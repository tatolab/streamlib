# One portable path for GPU work from Python, and a check that names the rest

Rationale for the `[portable-gpu-interop]` entries in `docs/plan/ARCHITECTURE.md`
§Packages and §Graphics, decided by the owner 2026-09-22. The facts behind them are measured and cited
in `docs/research/2026-09-22-portable-gpu-interop-for-python-processors.md`.

## Trigger

Read this before adding a Python dependency to the scaffold or to an example, before
teaching a device-selection idiom in docs or templates, before widening what a frame's
DLPack export accepts, before giving the stub per-platform blocks, and when someone asks
why cupy or MLX "is not supported" or why the check only warns.

## The decision

1. **torch is the one portable path for GPU math in a Python processor.** The engine mints
   a DLPack capsule whose device is the floor's own — CUDA on Linux, Metal on macOS — and
   torch is the only library that reads both, zero-copy. The device comes from the tensor
   or from `torch.accelerator`; a device spelled as a literal is what makes code
   single-floor, so it is never modelled.
2. **Every other DLPack consumer is allowed on the floor it supports, and never refused.**
   cupy, jax and MLX are real users' real tools. Interop is a promise to any DLPack
   consumer; portability is a promise about one named path. Holding both apart is what
   lets neither be weakened.
3. **The scaffold stays free of torch.** torch on the Linux floor is roughly two gigabytes
   with its CUDA dependencies; the first minute cannot carry that. torch belongs in the
   examples that do tensor work.
4. **The host side of a frame is one line on both floors.** A DLPack request for the CPU
   device hands back the host mapping — on macOS the same pages the Metal capsule aliases,
   so it is not a copy. numpy and jax cannot read a Metal capsule, and this is how they
   read a frame at all there. A request for a copy stays refused: an exporter that hands
   its own memory to a consumer believing it owns a private copy is a latent corruption.
5. **A cross-floor check, not a new command.** Packaging already stops native code and
   most floor-bound dependencies at install; nothing stops a pure-Python processor that
   names `"cuda"`. The check reads source and dependency markers and names the portable
   spelling. It runs where users already are — `streamlib dev` / `run` — and warns
   without blocking, because guardrails inform rather than wall off; it gates only what
   the project ships. It is named apart from the portability gate, which is about native
   linkage.
6. **The engine copies surfaces for Python.** The only reason the GPU examples imported an
   array library at all was to land a frame in a kernel's input texture — one line that
   bound them to one floor. Every engine surveyed ships that copy as a first-class call,
   and the RHI already has it; exposing one general surface-to-surface copy makes that
   line portable by construction and cheaper than the blit-out, foreign import and
   blit-back it replaces. It is general rather than frame-to-texture only because which
   copy two backings need is the engine's business, not the method name's.
7. **The stub stays one surface.** A wrong-floor call refuses by name and names its peer;
   the cross-floor check gives the early signal the stub would otherwise be split to give.

## Rejected alternatives

- **The Python Array API (array-api-compat) as the modelled path** — it makes math
  portable but not devices: it cannot name "the GPU", excludes custom kernels, and still
  needs a library that reads the capsule, which is torch on both floors. Allowed, not
  modelled.
- **Both torch and the Array API, with a rule** — two idioms to teach and check for one
  concern.
- **MLX as a portable path** — its CUDA backend refuses CUDA DLPack both ways; it is the
  macOS peer of cupy, not a cross-floor library.
- **torch in the scaffold** — the install weight breaks the first minute.
- **ruff's banned-API rule as the check** — misses literals, `.cuda()` and the closed-list
  names; ruff has no plugin system and is not otherwise used here, so it would make a
  second tool the authority for one concern.
- **A dedicated CLI verb** — ceremony for users; the check belongs where they already run.
- **Blocking user code on a finding** — a single-floor app is a legitimate choice.
- **Per-platform blocks in the stub** — splits one surface into two, trades the refusal's
  named peer for a bare missing-attribute error, and only reports on the floor the type
  checker runs as.
- **A frame-to-texture-only copy** — a second method the day a kernel's output needs
  copying on; the backing pair is the engine's to resolve.
- **A converting blit** — colour conversion is a second concern inside what should be a
  copy.
- **Honouring a copy request in the export** — a new ownership story for no consumer that
  needs it; numpy asks for the host device, not a copy.

## Consequences

- The shipped GPU examples owe a conversion off cupy and off device literals, carried by
  the change that schedules it; the scaffold does not change for this decision.
- Every DLPack export arm on every floor honours the host-device request, and each new
  floor's arm must.
- The cross-floor check's findings are advisory for users and blocking for the project's
  own Python; its reach is source, so the same suite on both CI lanes stays the backstop.
- A future floor that torch does not reach reopens the first decision.
