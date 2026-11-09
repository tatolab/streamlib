# studio-view-nodes

> **Decided by the owner, 2026-11-09.** A view on the canvas is a real graph node,
> `tatolab.studio:StudioView`, with `config.view_type` naming the view (the Node-RED proxy
> pattern). The runtime reads the view's declared ports and their families from the view contract
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

This change reshapes #3306 (a StudioView node) and #3307 (listing `views/` folders).

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

- **The `StudioView` built-in.** A native built-in, `tatolab.studio:StudioView`, takes
  `view_type` (the view's id, `<project or import folder>/<view name>`) and the view's settings as
  its config. At load the runtime reads that view's manifest and gives the node one input per
  declared input and one output per declared output, each of its declared family. A view that is
  missing or does not open fails the node by name, the way an unregistered `type` is refused.
- **Panel reads.** A panel reads a `StudioView` node's inputs through `open_viewer` on the port
  linked into it. An Image stays zero-copy, and the node may hold one GPU copy of an inbound Image
  where the view keeps a frame.
- **Panel writes.** `write_studio_view_output {stream, node, port, value}` hands a value to the
  node. The node owns its outputs' values and publishes each new one on that port, and a
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
  type mirroring the view SDK's Zod schema. A CI gate checks that type against the schema the view
  SDK generates, so a drift fails the build. The Zod schema stays the guarantee, and the generated
  JSON Schema serves only the drift check. Owner, 2026-11-09: "runtime owns it".
  Rejected: Studio telling the runtime a view's ports when it places the node — the graph would
  depend on Studio being open, and an agent-built graph would have no ports (owner, 2026-11-09).
- **Studio's Frame is a group.** The runtime's labelled group, a title, a description and a colour
  that nodes join, is what Tato Studio draws as a Frame. It groups nodes and does nothing else, and
  no second canvas item exists for it. Owner, 2026-11-09: "it's the same, just a grouping of
  things".

## MODIFIED

- §Processor model: a new entry for the `StudioView` built-in, its ports read from its view's
  manifest, and the panel write path.
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
