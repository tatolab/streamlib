# The MVP sentence — why this shape

Rationale for the `[product-mvp-sentence]` entries in `docs/plan/ARCHITECTURE.md`
§Product, decided in an /align session on 2026-07-29.

## Trigger

Read this when scoping work against the MVP sentence, when tempted to widen the MVP's
audience/platform/language, or when a "simpler" proposal reintroduces a manifest, an
entry-point config, or an engine-side schema check.

## Decision

The MVP promises the **developer experience**, not the capability. Camera → processor →
display pipelines, shader/compute processors, and packages already exist and work; what
does not exist is the simplified experience around them. The sentence:

> A Python developer on Linux with an NVIDIA GPU pip-installs streamlib, runs
> `streamlib new` then `streamlib dev`, sees their camera live in a window within a
> minute, and makes the pipeline theirs by editing the scaffolded processor — zero
> ceremony: no manifest, no `main()`, no schema wrangling, a fast edit loop.

(Sentence updated 2026-08-02 by `importable-python-library`: "from PyPI" → pip-install
from repo releases until the rename; "hot-reload on save" → "a fast edit loop" —
re-running `dev` is the MVP loop; processor-granular reload-on-save is a nicety,
never module machinery.)

Its load-bearing terms:

- **Python consumer.** Python vision/ML developers are the audience for realtime GPU
  pipelines without writing Vulkan.
- ~~**PyPI ships the single binary** (CLI + runtime + build orchestration) — the
  ruff/uv pattern.~~ — Superseded 2026-08-02 by `importable-python-library.md`. PyPI
  ships one *wheel* — Python API + CLI + engine via PyO3, the pydantic-core pattern;
  build orchestration is deleted. Still one install, no toolchain.
- **Batteries-included scaffold.** `streamlib new` generates a *working* camera →
  effect → display app with dependencies already installed; first `streamlib dev` shows
  live video before any code is written; the user's first act is editing the scaffolded
  effect processor already wired into the pipeline. This create-next-app moment is most
  of what "amazing DevEx" means, and it is the integration piece that was missing from
  the CLI-forward design (which scaffolds plugins only).
- **`app.py` + `setup(rt)` by convention, `-f` override.** Matches what Python
  developers rely on (Flask defaults to `app.py`; FastAPI's `uvicorn main:app` is the
  explicit variant); the override keeps the point-at-a-script launch.
- **Apps carry no manifest.** ~~The npm model: `add`/`link` write `streamlib.lock`,
  `streamlib_modules/` holds the installed set, and the identity label exists only
  because a package is shared. An app promotes to a package by adding the label.~~ —
  Superseded 2026-08-02 by `importable-python-library.md`. Stronger now: the pip/uv
  model, no streamlib-specific files at all — `pyproject.toml` and the venv are the
  whole dependency story; a processor package is an ordinary PyPI package.
- ~~**App-local processors use the plugin discovery scan** on the app's `processors/`
  folder, minted `@app/local/<Name>`; execution stays in the engine-spawned
  subprocess.~~ — Superseded 2026-08-02 by `importable-python-library.md`. App-local
  processors are ordinary Python classes imported into `app.py`; `rt.add` takes the
  class; no discovery scan, no minted ids. (This replacement's own "execution placement
  (in-process or a same-interpreter helper process) is the engine's decision" clause was
  superseded 2026-08-04 by `helper-process-placement-only.md`: there is one placement — a
  helper process per Python processor — and it is not a decision. "Imported into
  `app.py`" is now a hard requirement, not a convenience: the class must live in an
  importable module, so `app.py` imports it rather than defining it.) The surviving point: the
  class form keeps go-to-definition and type checking working, which is what makes the
  loop agent-friendly (small `app.py`, one obvious processor file, `graph`/`tap` to
  verify an edit landed).
- **`add`/`connect` is the pipeline API** — the existing primitive, explicit about
  ports.

## Rejected alternatives

- **Polyglot MVP** — an MVP addressed to everyone ships for no one. Rust authoring stays
  a supported capability for hardware-facing packages (C++ later), outside the sentence.
  TypeScript is deliberately de-prioritized: live-visual-effects users are better served
  eventually by processors exposing web-based authoring than by a first-class TS
  consumer story.
- **Broader platform floor (any Vulkan GPU, macOS)** — promises verification work that
  does not exist; Linux + NVIDIA is where the code is.
- **curl-installer as primary channel** — worse first touch for a pip-native audience;
  can be added later.
- **Minimal (empty) scaffold** — kills the first-run payoff; the zero-to-video minute is
  the pitch.
- **Entry point named in a config file** — ceremony returning through the side door.
- **Every app is also a package** — a consumer needs no publishable identity; forcing
  one re-creates the manifest ceremony being removed.
- ~~**Import-only or strings-only local processors** — import-only forks the discovery
  model from plugins; strings-only loses IDE/agent ergonomics.~~ — Superseded
  2026-08-02 by `importable-python-library.md`: import-only (class-form `rt.add`) IS
  the decided shape; the discovery-scan model it would have "forked from" is deleted,
  and IDE/agent ergonomics are what the class form provides.
- **Higher-level pipeline API now** (chaining sugar, declarative graphs, or a
  Holoscan-style `compose()` subclass) — the subclass shape is *more* ceremony than
  `setup(rt)`; sugar remains compatible later where port inference is unambiguous.

## Consequences

- The zero-ceremony bar makes the schema-agreement rip-out (no engine schema matching,
  cast-at-read, no versions at the code layer) MVP-blocking work, not cleanup.
- `streamlib new` (app scaffold) is a new command to design and build.
- ~~Existing Rust plugins port to the new format as the final step, so they install as
  modules; consumer examples continue to lag by design until then.~~ — Superseded
  2026-08-02 by `importable-python-library.md`: there is no plugin format to port to;
  first-party media becomes engine-tree built-ins, and other plugin functionality is
  re-authored as Python packages or cargo crates per the pivot's change file. (Narrowed
  2026-09-04 by `extension-model.md`: the built-ins that shipped stay; a further
  first-party capability ships as an extension wheel unless the criterion admits it.)
- The MVP claim is narrow (one persona, one platform, one GPU vendor) and must be kept
  honest: widening any axis is a plan change, not a ticket.
