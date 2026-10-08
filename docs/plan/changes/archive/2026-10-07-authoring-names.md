# authoring-names

The authoring surface's two names that collide get explicit spellings, with no alias, in the
milestone that already makes every author re-import everything:

- the object a `@stream` function is handed is a `StreamBuilder`, and every place that models the
  parameter spells it `stream_builder` —
  ```python
  from tatolab.stream import CameraSource, DisplayWindow, StreamBuilder, stream

  @stream
  def main(stream_builder: StreamBuilder) -> None:
      camera = stream_builder.add(CameraSource)
      window = stream_builder.add(DisplayWindow)
      stream_builder.connect(camera.output("video"), window.input("video"))
  ```
- a node declares its ports with `@node.input` and `@node.output`, attributes of the `@node`
  decorator, so nothing a node module imports shadows Python's builtin `input()` —
  ```python
  from tatolab.stream import RuntimeContextLimitedAccess, VideoFrame, node

  @node
  class InvertingEffect:
      @node.input(delivery_profile="newest")
      def video_from_upstream(self) -> VideoFrame: ...

      @node.output()
      def video_to_downstream(self) -> VideoFrame: ...
  ```

Unchanged: the `@stream` and `@node` decorators; a node reference's `input(name)` /
`output(name)` (a method on its receiver shadows nothing); the Rust attribute macro's port keys
`input(…)` / `output(…)`, already the same word; the port model itself — name,
description, delivery profile, the audio window contract (§Processor model); the wire and every
graph key; the engine's Rust identifiers.

**Scale gate — this skill plus an ADR.** The Python API's public contract
surface changes spelling. The rationale is `docs/decisions/tatolab-names.md`, §"Decided
2026-10-06: the builder and the port decorators". No behaviour changes: a pure rename, shipped
as a breaking change with no alias (pre-1.0).

**Precondition.** Every entry built is DECIDED: §Packages, "The authoring names, with no alias"
(owner, 2026-10-06), which amends §Product `:45-55` and `:166-174` and §Packages' package-split
entry (`:549-567`). Sequencing: after #2586 (merged, PR #2673); ahead of the stand-alone stream
package (#2588), which moves `tatolab/stream/` and most of the tests below wholesale — renaming
first edits each file once, in place.

**Verified against the tree 2026-10-06 (HEAD d7f680741).**
- `@stream` hands the builder positionally — `compile_stream_to_graph` does `builder =
  Stream(stream_name)` then `stream_function(builder)`
  (`sdk/streamlib-python-wheel/python/tatolab/stream/_stream_graph_builder.py:369-384`), and the
  decoration check (`:150-160`) requires one positional parameter and never reads its name. Only
  the refusal text (`:64-67`, `:157`), `tatolab/runtime/cli.py:120-131`
  (`STREAM_FUNCTION_EXPLAINED_WITH_A_SAMPLE`), the scaffold template
  (`tatolab/runtime/_scaffold_template/stream.py:21`) and `README.md:116` model `stream: Stream`;
  `tests/test_cli.py:128`, `:828`, `:864`, `:883` assert that text.
- The class is `Stream` (`_stream_graph_builder.py:232`), exported from `tatolab.stream`
  (`tatolab/stream/__init__.py:150`); `cli.py:795` lists `"Stream"` among the scaffold's expected
  imports.
- The decorators are `def input(` and `def output(` (`tatolab/stream/_processor_declaration.py:254`,
  `:313`), re-exported as `input as input  # noqa: A004` and `output as output`
  (`tatolab/stream/__init__.py:60`, `:62`). Only `input` shadows a Python builtin.
- Counts outside `examples/`, the plan, ADRs and changelogs: `@input(` 96 lines in 39 files,
  `@output(` 70 in 27, `stream: Stream` 158 in 63, a `Stream` import or annotation about 190 lines
  in 107 files.

---

## MODIFIED: §Product and §Packages — the spellings

- §Product `:45-55`: "`stream.add` takes the class; the builder's API is `add`/`connect`/`expose`"
  reads `stream_builder.add`, the builder being a `StreamBuilder`.
- §Product `:166-174`: the worked example is `@stream def camera_rig(stream_builder: StreamBuilder)`.
- §Packages' package-split entry (`:549-567`): `tatolab.stream` carries `@node.input`,
  `@node.output` and the `StreamBuilder`.
- GLOSSARY "Stream (user)" and "Node reference" already name the `StreamBuilder` and
  `stream_builder.add`.

## MODIFIED: the Python surface (`sdk/streamlib-python-wheel/python/tatolab/stream/`)

- `class Stream` → `class StreamBuilder`; every annotation, `__all__` entry, docstring, refusal and
  CLI sample follows. The refusal and sample text model `def main(stream_builder: StreamBuilder)
  -> None:`.
- `def input(` / `def output(` stop being module-level exports and become attributes of `node`:
  `node` turns from a function into a typed callable object whose `__call__` keeps today's
  `@node` / `@node(...)` behaviour and whose `input` / `output` are today's port decorators, so
  pyright resolves `node.input(...)` and its keywords. The re-exports at `__init__.py:60`, `:62`,
  their `__all__` entries, the `noqa: A004` and its comment go.
- The scaffold templates (`tatolab/runtime/_scaffold_template/`) render `stream_builder:
  StreamBuilder` and `@node.input` / `@node.output`; ruff and pyright over every render as today.
- `_engine.pyi` and every docstring that spells `@input(` / `@output(` / `Stream` follow.

## MODIFIED: callers in the engine tree

- `sdk/streamlib-python-wheel/tests` (the bulk of every count above), the engine fixtures under
  `runtime/streamlib-engine/tests/fixtures`, Python held in Rust test strings
  (`sdk/streamlib-python-wheel/src`), `packages/streamlib-webrtc` (as #2586 moved it, the canary
  §Consumers reserves), `README.md`, `docs/architecture/`.
- Held pre-pivot consumers (`packages/clap`, `packages/jpeg`, `packages/screen-capture`) lag by
  design and are not edited. The converted `examples/` are backlog filed at ship.

## Inventory — what the old spelling leaves, and the slice that ends it

| Old spelling | Ends | Slice |
|---|---|---|
| `class Stream`, its export, the `stream: Stream` sample, refusal and scaffold text, every annotation and test | renamed `StreamBuilder` / `stream_builder` | A |
| `def input(` / `def output(`, their re-exports and `noqa: A004`, every `@input(` / `@output(` in the wheel, tests, fixtures, webrtc and docs | moved onto `node` as `node.input` / `node.output` | A |

## Slices, as ticketed

Derived 2026-10-06, milestone "Native runtime, engine-free stream venvs": A is #2679 (blocks #2588).

- **A — the Python surface.** `StreamBuilder` / `stream_builder` and `@node.input` /
  `@node.output` across the wheel, its tests, the engine fixtures, `streamlib-webrtc`, README and
  docs. Mechanical; one session without ultracode. Proof: stubtest, pyright, the GPU-free suite
  on both lanes, the scaffold renders and lints, the webrtc lane; the `requires_gpu` half once on
  the rig. Blocks #2588.

## Left to later changes

| Not here | Because | Lands with |
|---|---|---|
| `examples/` | converted consumers | backlog filed at ship |
| `packages/clap`, `packages/jpeg`, `packages/screen-capture` | held pre-pivot consumers | their own disposition |

## Assumptions stated, not asked

- The node reference's `input(name)` / `output(name)` keep their spelling (decided in the entry).

## REMOVED

- REMOVED: class Stream:
- REMOVED: stream: Stream)
- REMOVED: @input(
- REMOVED: @output(
- REMOVED: input as input
