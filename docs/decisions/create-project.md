# Creating a project: the runtime scaffolds, makes the venv and loads, under a generated name

Rationale for the `[create-project]` entries in `docs/plan/ARCHITECTURE.md` (§Product, with the
runtime unit's uv in §Distribution & versioning), decided 2026-10-28.

## Trigger

Read this before adding another way to start a new project (an app page, a CLI verb, an agent
tool), before giving a new project a default directory or name, before making a client run
`uv` or carry its own Python for a project, and before letting a load refuse a project only
because its `.venv` has not been made yet.

## Decision

- Creating a project is one runtime operation: write the scaffold, make the project's `.venv`,
  then load and record the scaffold's one stream as every load is. A
  project holds any number of streams, so the operation is named for the project. `tatolab new`, the `create_project`
  tool and `Tatolab.app`'s Add stream › Create all call it, so a project an agent made is
  indistinguishable from one a person made, and a later session finds it in `list_streams`.
- The caller names the directory; the runtime cannot see an MCP host's working directory,
  because `tatolab mcp` is a byte pipe. With none, the runtime draws an adjective-noun name
  (`quiet-otter`) and creates `~/streams/<name>`, drawing again while the directory exists.
  The runtime owns the word lists, so every client proposes names the same way.
- A created project's display name is its folder's name, because every scaffolded stream's
  function is `main` and a list of `main`s tells a person nothing. Renaming changes only the
  label; the folder is the location and stays where it is.
- Create refuses, before writing anything, a directory holding any file the scaffold would
  write. A directory that already holds a project is added by loading it, never by creating.
- The runtime unit carries a pinned uv on both floors, and the runtime makes every `.venv`
  with it: create always does, and a load of a project with no `.venv/bin/python` does it
  first. uv takes the Python the project's `.python-version` pins from the machine or fetches
  it.
  > ~~A `.venv` holding `bin/python` is never touched.~~ — Superseded 2026-10-29 by
  > `load-never-refused-for-code.md`: the runtime makes the `.venv` whenever its interpreter
  > does not run, on every load, start, restart, create and listing.

## Rejected alternatives

- **`hello`, then `hello-2`, `hello-3`, …** — the app's prototype default; it reads as a
  placeholder, and the suffix grows forever on a machine that makes many projects (owner).
- **An adjective-noun name with a numeric suffix (`quiet-otter-4821`)** — the stream id
  already carries a hash, and the folder only has to be free on one machine; the suffix only
  adds noise.
- **A separate create verb beside `tatolab new`** — two verbs that both write the scaffold and
  differ only in whether the stream is loaded; `tatolab new` followed by `tatolab dev` stays the
  product sentence's path, with `dev` following a stream that is already loaded.
- **The runtime unit carrying Python as well as uv** — `uv sync` needs the network for PyPI
  anyway, so a bundled interpreter saves one download at a much larger unit, and couples every
  Python patch release to a runtime release.
- **uv from the user's `PATH`, refused when absent** — Claude Desktop, with no terminal, could
  not create a project on a machine without uv, which is the case this exists for.
- **Create adopting a directory that already holds a project** — create would become "create or
  add", and a mistyped directory would load someone's project instead of refusing.
- **A load refusing a missing `.venv`, pointing at `uv sync`** — an agent with no file access
  could not add a cloned project, and every client would keep its own copy of the sync step.

## Consequences

- The scaffold templates move from the CLI into the runtime, which serves them; the CLI's
  `tatolab new` becomes a local-API call and, like every verb, fails with no runtime running.
- `Tatolab.app` stops scaffolding, stops syncing and stops shipping its own uv and Python; its
  Python picker has no runtime counterpart. That work lands in the app repository.
- Creating a project and a first load need the network: uv fetches the pinned Python when the
  machine has none, and the project's dependencies from PyPI.
- A uv-fetched Python on macOS is outside the app bundle's signature, as any terminal user's
  Python is; processor interpreters already run from the project's own `.venv`.
