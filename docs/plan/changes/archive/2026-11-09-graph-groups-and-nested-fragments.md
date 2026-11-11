# graph-groups-and-nested-fragments

> **Decided by the owner, 2026-11-08:** Studio shows a subgraph "somewhat like effect house and
> others where you can have breadcrumbs and navigate down to subgraphs and in the level where a
> subgraph is used we have a singular node showing that its running a subgraph so you're seeing
> the input and output pins simplified"; and labelled groups: "in unreal and other engines you
> can highlight a set of nodes and provide basically a color label and description so other
> people know what a group does, that has no impact execution but its helpful to store inside our
> system". Collapsing nodes into a subgraph that stays hand-editable is designed with loop zones.

A small addition to project-and-assets, which already makes a subgraph a fragment. The Composition
milestone written for `@stream` subgraphs is retired with it: its routing toggle and preview were
overtaken by gateway-nodes (#3213, #3214) and runtime viewers (#3215).

**The direction, in four sentences:**
1. A generator may place another generator, so a fragment may hold fragments, nested as deep as
   the graph document's container limit allows; a generator that places itself, directly or
   through another, is refused by name when it runs.
2. A nested fragment flattens at compile under its path of reference names
   (`look/blur_then_tint/blur`), and `graph` renders it as a container inside its container, so
   Tato Studio shows one node with the fragment's pins at each level and steps inside with
   breadcrumbs.
3. A labelled group is a record in the graph document — a UUID, a title, a description and a
   colour — that nodes belong to; it changes nothing that runs and is saved like studio fields.
4. A container a person draws or collapses by hand, which a generator does not own, is not built
   here; it is designed with loop zones, which need the same thing.

**What an author writes after this change:**

```python
@graph_generator
def blur_then_tint(graph, radius: int = 4):
    source = graph.add(GraphInput, config={"port": "video", "family": Image})
    blur = graph.add(Blur, config={"radius": radius})
    tint = graph.add(Tint)
    graph.connect(source.value, blur.video)
    graph.connect(blur.video_out, tint.video)
    graph.add(GraphOutput, config={"port": "video", "family": Image}, value=tint.video_out)

@graph_generator
def look(graph):
    source = graph.add(GraphInput, config={"port": "video", "family": Image})
    soft = graph.add(blur_then_tint, name="soft", radius=8)
    graph.connect(source.value, soft.video)
    graph.add(GraphOutput, config={"port": "video", "family": Image}, value=soft.video)
```

## ADDED

- **Nested fragments.** `graph.add` inside a `@graph_generator` takes another
  `@graph_generator` as it takes a node class: its keyword arguments are the inner generator's
  promoted config, and the reference it answers carries the inner fragment's Input and Output
  ports by name. The import or refresh of the outer fragment runs the inner generator in the
  same interpreter and stores its output as a fragment inside the outer one. A refresh of the
  outer fragment re-runs every generator under it; a node keeps its UUID while its path of names
  is unchanged.
- **Self-placement refused.** A generator that is already running higher in the same import or
  refresh is refused when it is placed again, naming the chain
  (`look → blur_then_tint → look`); nothing of that import is written.
- **Nesting depth.** A fragment nests as deep as the graph document's container limit allows
  (serde_json's 127 nested containers, the limit
  `_MOST_CONTAINERS_A_GRAPH_NESTS_FROM_ITS_ROOT` already counts in
  `sdk/tatolab-stream/tatolab/stream/_stream_graph_builder.py:73`); a deeper one is refused at
  import, naming the fragment and the depth, before the document is written.
- **Groups.** The graph document gains `groups`: each `{id, title, description, colour}`, its `id`
  a UUID the runtime generates. A node's `studio.group` names a group's UUID, or is absent. A
  group holds nodes of one level — the stream's or one fragment's — and a fragment's own nodes
  may belong to groups inside it. Groups are saved, rendered by `graph` and read by Tato Studio;
  the compiler never reads them.
- **The group operations**, each a transaction as #3258 defines one: `add_group {stream, title,
  description, colour, nodes}` (answers the UUID; sets each named node's `studio.group`), `set_group_fields`, `remove_group` (its nodes
  stay, ungrouped; its inverse carries the group and the UUIDs of every node that belonged to it,
  as `remove_node`'s carries its node and links). Membership lives only on the node, in
  `studio.group`; a group holds no node list. A node joins or leaves a group through `set_studio_fields`. A group inside a
  fragment comes only from its generator, through `graph.group(title, nodes, description=,
  colour=)`, and is read-only like the fragment.

## MODIFIED

- **Fragments** (project-and-assets §ADDED): a fragment may hold fragments, refreshed with it.
- **The graph document** (project-and-assets §ADDED): it holds `groups`; a node's `studio.group`
  is a group's UUID, never a free label. The golden document
  `runtime/streamlib-media-builtins/tests/golden_graph_documents/0001-…graph.json`, which sets
  `"group": "sources"`, keeps loading: a golden is never edited, so a `studio.group` that names no
  group loads with the node ungrouped, and a new golden beside it holds a group.
- **§Processor model, the nesting OPEN** becomes DECIDED as sentences 1 and 2 above.

## REMOVED

Nothing in the tree; the Composition milestone prompt was never committed.

## Decisions

**Decided 1 — nesting is bounded only by the document (owner, 2026-11-08).** A recursion guard
by name is enough; an arbitrary depth cap would refuse a real graph a reader can parse.

**Decided 2 — groups change nothing that runs (owner, 2026-11-08).** They are the designer's
labels, as Unreal's comment boxes, Houdini's network boxes and TouchDesigner's annotations are,
and nothing reads one to schedule, place or name a node.

**Deferred, designed with loop zones:** a container a person draws by hand or collapses from a
selection, editable with the graph operations and owned by the document instead of a generator
(Unreal's Collapse Nodes); and unlocking a fragment into one.

## Tickets (as few tracer bullets as the change needs)

1. **#3259 — Nested fragments** — folded into #3259 as one more Done-means line, because the fragment and
   its refresh are built there once: a generator placing a generator, the chain refusal, the
   depth refusal, the nested container in `graph`. Waits on #3257 as #3259 does.
2. **#3308 — Groups in the graph document** — `groups`, `studio.group` as a UUID, the three operations,
   `graph.group` in a generator, a golden document holding a group. Waits on #3258.

## Notes (not in scope)

- tatolab-app #134 (step inside a subgraph) reads the nested containers `graph` renders; Tato
  Studio's group frames read `groups`. Both are app issues that wait on these tickets.
- Loop zones (*Loops for function nodes: options*, owner's pick pending) are the next design
  step after this one.
