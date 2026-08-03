# Importable Python library — the SDK-shape pivot

Rationale for the `[importable-python-library]` entries across `docs/plan/ARCHITECTURE.md`,
decided by the owner 2026-08-01, direction confirmed verbatim 2026-08-02. Supersedes parts of
`single-binary-launch.md`, `media-io-layering.md`, and `product-mvp-sentence.md` (annotated
in place).

## Trigger

Read this before reintroducing any custom distribution mechanism (module folders, install
verbs, runtime downloading), before proposing a new dlopen/ABI extension path, before putting
Python in a per-frame media path, or when someone asks why streamlib is a wheel and not a
framework.

## The direction, verbatim (owner-confirmed)

1. StreamLib becomes an importable Python library: `pip install streamlib` delivers one PyO3
   wheel containing the Python API and the Rust engine, and a StreamLib app is a normal Python
   codebase — one uv-managed venv, one Python version, ordinary PyPI dependencies, no manifest,
   no custom module system (`streamlib_modules/`, add/install/link/pkg, and runtime downloading
   are deleted).
2. Processors are Python classes — written in the app or imported from pip-installed packages —
   and `rt.add` takes the class; whether the engine runs a processor in-process or in a helper
   process spawned from that same interpreter and venv is an under-the-hood engine placement
   decision, never a user-facing runtime definition.
3. The plugin ABI is deleted entirely: third-party native code ships as ordinary Python packages
   that expose handles (frames, FDs, device pointers) to a Python processor and never speak
   streamlib internals, while first-party camera, display, and audio ship inside the wheel as
   native pre-built blocks driven from Python by configuration.
4. Python is the sole focus runtime: cross-language interop happens on the wire between mesh
   nodes as self-describing bags — never in-graph — so the current Deno SDK is deleted with the
   subprocess machinery it's built on, while TypeScript authoring is paused-not-rejected (a
   future importable TypeScript SDK on this same model), and a Rust app remains a plain cargo
   project on the `streamlib` crate.
5. The CLI slims to `new` / `dev` / `run` as a thin runner plus the observation verbs
   (`nodes` / `graph` / `tap` / `logs` / `mcp`); build orchestration, packaging, provisioning,
   and codegen verbs are deleted — compilation happens at publish time with standard tools, and
   JTD schemas are demoted to advisory, experimental type info behind a rip-out-cheap seam,
   never a requirement for accessing data.

## Why

**The pain was never processes — it was environments.** What the framework shape forced was an
interpreter zoo: per-processor venv provisioning, a custom module system (`streamlib_modules/`,
`.slpkg`, install/link), runtime downloading, and compile-at-the-destination machinery
(`BuildOrchestrator`). That was the community-hostile, bespoke layer. The ideal is one venv,
uv, PyPI, one Python version, a codebase that feels like any normal Python app. Whether the
engine places a processor in-process or in a helper process spawned from the same interpreter
is an implementation detail — the measured gap (subprocess p50 0.161ms vs in-process 0.085ms at
720p60) makes both placements viable, so placement is demoted from architecture to engine
policy. Distribution, execution, and polyglot reach are three separable axes; the decision is
about the first.

**The plugin ABI's tenants all left.** First-party media bakes into the wheel; Python packages
extend via CPython's own stable ABI (the numpy/pydantic-core model — compiled internals, no
streamlib linkage); Rust apps compile processors from source. Of the ABI's 22 vtables, ~20
existed only to let a dlopen'd plugin reach engine GPU primitives without sharing a Rust type —
in one process those calls are ordinary Rust. The closed-source-vendor case is served by the
Python-package path: native internals expose transferable handles (DMA-BUF FD, CUDA device
pointer, buffers) and a Python processor wraps them. There is exactly one authoring surface,
and one engine per process.

**Native built-ins, Python everywhere else — the evidence.** Three independent analyses
converged: (a) the engine's primitive surface is already handle-shaped — the old media packages
crossed the ABI with fds, u64 handles, byte buffers, and POD, so the public interop contract
costs nothing new; (b) with one shared interpreter, a vsync-paced present (`begin_frame` blocks
up to a full refresh by design) or a device audio callback under the GIL stalls the entire
Python plane — display and audio device loops must be native; camera is Python-viable only
under a strict GIL-release discipline that does not yet exist in the SDK; (c) no surveyed
in-process realtime system (Holoscan, GStreamer) runs Python per frame in first-party
capture/present/audio — Python composes, native code moves frames; the one Python-media system
(dora-rs) buys it with process-per-node isolation and still went native for codecs. Dogfooding
favors shipping built-ins in the shape vendors are told to use — which the handle-shaped
contract satisfies without putting Python in deadline paths.

## Rejected alternatives

- **Keep the framework + custom module system** — the bespoke distribution layer was the
  fragility; every cited Python dev-tool win is an importable library, not an IoC runtime.
- **In-process as dogma (kill subprocess machinery outright)** — conflates the execution axis
  with the distribution axis; same-interpreter helper processes preserve isolation for
  blocking-heavy processors at no DX cost. What dies is per-processor *environments*, not
  processes.
- **First-party media as Python processors ("eat our own cooking")** — disqualified for
  display (vsync block under the GIL freezes every Python processor, every frame) and audio
  (never-block-never-malloc vs GC and unbounded GIL waits); the dogfooding value is captured
  instead by first-party Python transforms/effects and by the handle contract being the same
  one built-ins use.
- **Keep a narrow ABI for closed-source vendors** — resurrects the 22-vtable maintenance
  surface for a user that does not exist; two engines in one process (a vendor wheel linking
  its own streamlib) is silent breakage; the answer to engine-internal GPU access is a
  conversation, not an ABI.
- **Retire TypeScript permanently** — the hobbyist / video-creator audience keeps it alive as
  a future target; what dies is the subprocess-polyglot substrate, and a future TS SDK is
  importable-library-shaped. Paused, not rejected.
- **JTD as the mesh contract** — codegen'd exact types were strong when we controlled
  compilation; on a wire between nodes we don't compile, the self-describing bag is the
  contract and cast-at-read already governs in-graph. JTD stays advisory and cheap to rip out
  (possibly replaced by Arrow later).

## Consequences

- The largest deletion in the project's history: plugin ABI (all vtables, handshake,
  fingerprints, layout tests), plugin-sdk cdylib arm, five `-abi` adapter crates, cdylib
  flavors, `BuildOrchestrator` + build-on-place + cargo-build + pack + `.slpkg`, the
  add/install/link/pkg/generate/schema/setup verbs, the Deno SDK and native cdylib, the
  standalone runtime binary, `streamlib_modules/` and the package source. Rip-out is
  inventoried by the pivot's change file, never kept running in parallel.
- The wheel becomes the build product: maturin/CI, abi3 across a small Python version range,
  camera/display statically linked. Our CI builds our wheel; nothing else is ever compiled by
  streamlib. Initial releases are repo-hosted wheel artifacts; PyPI publication is deferred
  until the project rename (name reservation before then would burn the throwaway name).
- Re-authoring the old packages and examples into the new shapes (built-ins absorption aside,
  which is rip-out work) is deferred to its own planning sessions and milestones after the
  wheel exists — dispositions are recorded in the pivot's change file, not ticketed now.
- First-party media moves *into* the engine tree; lag-by-design ends for built-ins.
- The Python SDK grows a load-bearing GIL-release contract (zero `allow_threads` exists today);
  the spike's latency numbers were measured with no blocking I/O in any callback and must not
  be cited for Python-hosted display/audio.
- One engine addition closes the display loop: a "present this surface (fit/fill,
  color-managed)" composition call, so the display block is config + one call.
- The interop adapters (Vulkan↔CUDA, Vulkan↔GL) survive minus their `-abi` halves; the
  torch/cupy zero-copy story is the largest remaining technical risk, now explicit.
- M39 is re-derived against this direction; the client-SDK/control-plane embedding story for
  Python is superseded by in-process embedding via the wheel.

## The authoring grammar, as built

Settled while implementing in-process authoring; recorded so the shape is not re-litigated.

- **Ports are class attributes, not decorated methods.** `frames_from_upstream =
  LinkInputDataPort()` makes the attribute name the port name, so a port is named once and
  `self.frames_from_upstream.read()` is a typed, completable expression. The alternative —
  decorating a stub method that exists only to carry metadata, then reading and writing by string
  through a context object — repeats every port name and gives an editor nothing to complete
  against. The engine binds a fresh per-instance port object when it constructs the processor;
  the class attribute stays a declaration, so two instances of one class read different links.
- **Lifecycle hooks take no arguments.** With ports on `self`, logging module-level and the clock
  module-level, a context parameter would carry nothing. `def process(self) -> None` is the whole
  signature.
- **`execution=` defaults to reactive only where reacting is possible.** A class declaring at
  least one input port defaults; one declaring none must say what it is. A source has nothing to
  react to, so the default would hand the author a processor that silently never runs — the one
  case where the convenient default is a trap.
- **Configuration is constructor keyword arguments.** `rt.add(Blur, config={"radius": 3})`
  constructs `Blur(radius=3)`, so a processor's settings are ordinary Python parameters with
  ordinary defaults and there is no configuration object to learn. It travels as JSON on the graph
  node rather than captured in a closure, because one class added twice must yield two
  independently configured instances — and because that keeps it visible in `graph`.
- **Python ports declare no schema.** The wire is self-describing and consuming is a cast at read
  time, so a port carries a name, a description and (on inputs) a delivery profile. Adding a
  schema hint here would build on the per-read matching being deleted.
- **Registration is idempotent per identity, and a collision is named.** Two different classes
  claiming one identity is refused with both qualified names and the fix, rather than surfacing
  the registry's generic duplicate error.
