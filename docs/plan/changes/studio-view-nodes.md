# studio-view-nodes

> **Decided by the owner, 2026-11-09.** A view on the canvas is a real graph node, and
> its type is the view: each view a project or one of its imports brings is a node type of its
> own, named `<project name>:<view folder>` (owner, 2026-11-10). The runtime reads the view's declared ports and their families from the view contract
> and reports them like any node's, so the canvas and Properties treat it first-class. The view's
> `settings` are the node's config, changed with `set_node_config`. Panels read the node's inputs
> through the viewer path, with Image zero-copy and one GPU copy allowed for an inbound Image.
> Panels write values to the node, which owns its values and publishes them. Downstream keeps the
> last value. Any number of panels show one node, and the last write wins. Views live in
> `views/<name>/` of a project and its imports, and the runtime lists them. A node's `studio`
> fields are a strict contract Tatolab owns and versions, for position, group, comment and colour,
> plus a free-form metadata map. Studio's feedback, confirmed by the owner: collapsed/expanded joins
> the fixed set, canvas Notes and Frames get their own place in the graph document, and the view
> listing carries the fields Studio named, including whether a view opens and why not. The view
> contract is the view SDK's Zod schema in tatolab-app, and a generated JSON Schema is a
> convenience, never the guarantee. The app-side record is tatolab-app `docs/engine-guarantee.md`
> (tatolab/tatolab-app#201).

This change reshapes #3306 (view nodes) and #3307 (listing `views/` folders).

## Current state (recon, `main` `a4574c2`)

- **The runtime knows no view.** No `StudioView` type and no `views/` reader exist in `runtime/` or
  `sdk/`. Tato Studio finds `tatolab.studio:StudioView` nodes by type in a graph it reads, and its
  view bridge refuses a view's output naming #3306.
- **A node's `studio` is a free map.** `studio: Map<String, Value>`
  (`runtime/streamlib-project-repository/src/stream_graph_document.rs:29-31`). Only `studio.group`
  is read (`:105`, `:432-470`), and `set_studio_fields` replaces the map whole (`:159-160`).
- **Groups are graph-level and labelled.** `add_group` makes a group with a title, a description
  and a colour, which a node joins through `studio.group` (`stream_graph_document.rs:90-96`).
- **Viewers read an output port with no copy** through `open_viewer` and `close_viewer`
  (`runtime/streamlib-api-server/src/mcp.rs:716-790`). Nothing lets a client write into a stream.

## ADDED

- **A view is a node type.** Each view a stream's project or one of its imports brings resolves
  as a node type of that stream, named by its view type (`<project name>:<view folder>`, as
  `list_project_views` lists it, such as `acme-dj:deck`), the same way a project's Python node
  classes resolve as types. The type's ports are the manifest's declared inputs and outputs, each
  of its declared family, and its description and port descriptions are the manifest's, so the
  node-type catalog reports them like any node's. A node of the type takes the view's settings as
  its config. A view that does not open is no type, so a node naming it is refused the way an
  unregistered `type` is. One node of a view per stream: adding a second node of the same view
  type to one stream is refused, naming the node that already holds it.
  Rejected: one `tatolab.studio:StudioView` type with `config.view_type` naming the view — one
  node type whose ports change with its config, where every user-defined thing elsewhere is a type
  of its own (owner, 2026-11-10).
- **Panel reads.** A panel reads a view node's inputs through `open_viewer` on the port
  linked into it, and an Image stays zero-copy. The runtime gives one viewer per port per
  connection, so Tato Studio's main process holds that one viewer and fans it out to every panel
  showing the port, and the runtime does not change for it (owner, 2026-11-10).
- **Panel writes.** `write_studio_view_output {stream, node, port, value}` hands a value to the
  node, an Image written from a panel taking at most one GPU copy into the node's own frame. The
  node owns its outputs' values and publishes each new one on that port, and a
  downstream reader keeps the last value. Many panels may write one node, and the last write wins.
  A write is live data, never recorded, since data on links is never saved (§Processor model).
- **Listing views.** `list_project_views {project_directory}` lists every `views/<name>/` of the
  project and of each import. Each entry carries its id, title, icon, description, preview,
  placement, declared inputs and outputs with their families, the project that brings it, and
  whether it opens, with the reason in the manifest schema's or the runtime's own words when it
  does not.
- **The `studio` contract.** A node's `studio` object is a versioned contract:
  `{contract, position, group, comment, colour, collapsed, metadata}`. The runtime checks every
  fixed key and keeps `metadata` as given. A graph document's own `studio` object holds the canvas
  items that are not nodes: `notes`, each with a UUID, text, position and size. These are edited by
  `add_note`, `set_note_fields` and `remove_note`, graph actions with inverses. Tatolab owns the
  contract, and a new version adds keys while the runtime reads every older one (owner,
  2026-11-09).

- **Reading a view's manifest.** The runtime reads a view's manifest file itself, with a serde
  type mirroring the view SDK's Zod schema. The Zod schema stays the guarantee. A CI gate holds the
  serde type to the view SDK's example manifests, good and bad: the runtime opens and refuses each
  one as Zod does, naming the same field. Owner, 2026-11-09: "runtime owns it".
  Rejected: checking the serde type against the JSON Schema the view SDK generates — a schema
  copy is not the contract, and the examples test the behaviour itself (owner, 2026-11-10).
  Rejected: Studio telling the runtime a view's ports when it places the node — the graph would
  depend on Studio being open, and an agent-built graph would have no ports (owner, 2026-11-09).
- **Studio's Frame is a group.** The runtime's labelled group, a title, a description and a colour
  that nodes join, is what Tato Studio draws as a Frame. It groups nodes and does nothing else, and
  no second canvas item exists for it. Owner, 2026-11-09: "it's the same, just a grouping of
  things".

## MODIFIED

- §Processor model: a project's views resolve as node types beside its Python node classes, each
  type's ports read from its view's manifest, and the panel write path.
- §Packages & extension model: a project's `views/` folder joins the project layout beside node
  code, and the view SDK's contract is the guarantee for a view, as `tatolab.stream` is for a node.
- §Control plane: `list_project_views` and `write_studio_view_output` join the tools.
  `set_studio_fields` sets the fixed keys and the metadata map instead of replacing a free map.

## REMOVED

- REMOVED: pub studio: Map<String, Value>

## Consumers

Tato Studio's view host finds view nodes by UUID, drives panels from the runtime's reported ports,
and sends writes through `write_studio_view_output` in place of its refused `send_output`, and draws a
runtime group for its Frame tool, which it calls a group. That is
tatolab-app #195 and #209, backlog that never blocks this change.
