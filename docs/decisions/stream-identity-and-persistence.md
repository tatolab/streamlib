# Stream identity and persistence: every loaded stream persists, identified by where it lives

Rationale for the `[stream-identity-and-persistence]` entries in `docs/plan/ARCHITECTURE.md`
(§Product, with the address in §Networking), decided 2026-10-27.

## Trigger

Read this before adding a verb or tool that loads, follows or names a stream; before letting
closing a connection, ending a terminal or exiting a host end a stream; before putting a
human-chosen label into an address; and before making a stream's id depend on load order,
on the label, or on anything but where its code lives.

## Decision

- A stream loaded into the runtime simply exists, however it was loaded: from `Tatolab.app`,
  `tatolab run`, or an agent's `run_stream`. Every load is recorded and comes back at every start.
  Following a stream — `tatolab logs -f`, `tatolab dev` — is the client's view of it, and
  ending the view never ends the stream; only `stop` and `rm` do. `keep`, `run -d` and the
  attached state go away.
- `tatolab run` loads and returns, as adding a project in the app does; it exits on whether the
  load was accepted.
- A stream has three things, each doing one job:
  - its **location** — the project directory and its stream function — is what it is; loading
    the same location again replaces it;
  - its **stream id** — `<folder>-<function>-<hash>`, computed from the location alone — is
    how it is addressed and keyed everywhere a handle must always work: the address, the
    relay broadcast, the record, the logs, every local-API tool;
  - its **display name** — a label the runtime records, renamable, not unique — is how a
    person reads it.
- The CLI and the app also take a display name only one stream holds, or infer the stream from
  the working directory, as conveniences over the id.

## Rejected alternatives

- **An attached stream bound to the connection that loaded it** (the 2026-10-01/02 shape) —
  Ctrl-C, a closed terminal or an MCP host quitting erased a stream, so an agent's next session
  could not find the project it was working on. `keep` was a terminal concept ("do I follow
  this?") that had become a lifetime concept ("does the runtime remember this?").
- **`run` following the stream, with Ctrl-C detaching only the terminal** — it also satisfies
  "it just exists", but it makes Ctrl-C look like stop; `dev` and `logs -f` are the verbs that
  follow.
- **A stream named by its function, a second load of the name refused with `--name` the way
  out** — the scaffold writes `def main`, so every scaffolded project collides once every load
  persists, and two worktrees of one project always do.
- **The display name in the address** — a rename would silently cut off every reader of the
  stream's ports.
- **A colliding name suffixed (`main-2`)** — the address would depend on load order, so an
  address an agent saved in one session could name a different stream in the next.
- **A random id, or one minted at first load** — it would not survive `rm` followed by a fresh
  load, and an agent could not find a stream again from where its code lives.
- **An id of the hash alone, or of the folder and hash** — the first means nothing in a URL or a
  log; the second cannot tell two streams of one project apart without reading the hash.

## Consequences

- Moving a project folder makes a new stream; the old record stays and fails to re-load until
  `rm` forgets it.
- An address is longer than a bare name (`hello-main-3f2a9c` rather than `main`), and stays the
  same across reloads and renames.
- `run`'s exit code reports only whether the load was accepted; a stream that fails later is
  read from `streams` or `logs`, as with `docker run -d`.
- `run_stream` refuses an argument it does not know, so a client still sending `keep` is
  refused once it is removed; `Tatolab.app` drops it in the same release.
