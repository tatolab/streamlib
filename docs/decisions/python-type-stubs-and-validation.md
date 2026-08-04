# Python type stubs and their validation

## Trigger

You are adding, changing or removing anything on the wheel's native surface (a `#[pyclass]`, a
`#[pymethods]` signature, a `#[pyfunction]`), or wondering why a hand-written `.pyi` sits next to
a compiled module, or whether to reach for a stub generator.

## Decision

The wheel ships a **hand-written `python/streamlib/_engine.pyi`** and a **`py.typed`** marker, and
two independent checks keep them honest:

- **`mypy.stubtest`** imports the built module, introspects it, and compares it against the stub.
  This is the drift check. It runs inside the wheel job, which already pays for the build.
- **`pyright` at `standard` mode** type-checks the package and its tests. With the stub checked in
  it reads no binary at all, so it runs as its own job with no Rust toolchain, no Vulkan and no
  maturin.

**A stub is part of the change, not a follow-up.** A new binding without a stub entry fails
stubtest, which is the intended pressure — the stub is the only thing an editor or a checker can
read about the native surface.

## Why this matters more here than for a pure-Python package

Nothing can be inferred from `_engine.abi3.so`. Measured on this tree: with the compiled module
present, pyright still reports `Runtime` as unknown and every derived type as obscured. Without
the stub, an author writing a processor gets no completion on `rt.`, no signature help, and no
checking — and an AI agent working in this repo sees the same nothing. The stub is what makes the
API legible to both.

`py.typed` is separate and equally load-bearing: without the marker a type checker ignores the
package's annotations entirely, however complete they are. It is inert at runtime and adds no
dependency — a user who runs no type checker sees no difference (PEP 561). What it does commit us
to is accuracy: the marker applies recursively, so a wrong stub is now actively misleading rather
than merely absent. That is the cost the two checks above buy down.

## Rejected alternatives

- **Generated stubs (`pyo3-stub-gen`)** — requires annotating every binding with companion macros
  plus a separate generator binary, and its own README states complete translation is impossible.
  No comparable library uses it in production.
- **Generated stubs (PyO3 `experimental-inspect` / maturin `--generate-stubs`)** — the right
  long-term answer, because a stub derived from the same macro metadata as the binary cannot
  drift. Not yet available: the PyO3 feature is explicitly in-development and inline-module-only,
  the maturin RFC is unmerged, and PyO3's own docs still name hand-maintained `.pyi` as current
  best practice. Revisit when `maturin build --generate-stubs` lands; adopting it would retire
  stubtest rather than supplement it.
- **`pyright --verifytypes` as the drift check** — it is not one. It scores annotation
  *completeness*, so a stub describing a method the binary no longer exports reports as fully
  complete and green. It answers "is everything annotated?", never "is the annotation true?".
- **Pyright at `strict`** — turns on `reportPrivateUsage`, which fights the `_engine` /
  `_NativeRuntime` shape the package is deliberately built on, and demands an annotation on every
  pytest fixture parameter. Measured: 104 errors, all but one in tests, nearly all cascading from
  a single unannotated fixture. `standard` over the same tree produced one diagnostic, and it was
  a real unguarded-`None` bug.
- **Shipping no stubs** — what NVIDIA's own realtime products do (Holoscan, TensorRT and
  DeepStream ship neither `.pyi` nor `py.typed`), and each has a years-old unresolved user
  complaint about it, worked around by third-party stub packages. The libraries whose users
  hand-write substantial code against a native core — PyAV, opencv-python, NVIDIA's own
  `cuda.core` — all ship full stubs.

## Consequences

- Every native-surface change costs a stub edit, enforced by CI rather than by review.
- Signature accuracy is worth stating precisely, not approximately: typing `Runtime.__exit__` as
  `Literal[False]` rather than `bool` is what lets a checker know a `with` block completed, and
  that one change fixed two possibly-unbound defects in existing test code.
- Pyright needs Node; `uvx pyright` handles that. If the download proves flaky on CI, basedpyright
  is a drop-in fork that ships Node in its wheel.
- The pyright job checks the tests too, so test code carries annotations. This is deliberate — the
  bugs it found were in test infrastructure, which is exactly where a silent failure hides longest.
