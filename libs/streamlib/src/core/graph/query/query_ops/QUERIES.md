# Query Operations to Implement

One operator per file, following vertex_op.rs pattern.

## Entry Points (QueryBuilder)
- v() / v(id) - start from vertices (vertex_op.rs - done)
- e() / e(id) - start from edges (edge_op.rs - done)

## Vertex Traversals (ProcessorQuery → ProcessorQuery)
- downstream() - get downstream vertices (skip edges)
- upstream() - get upstream vertices (skip edges)
- first() - narrow to first node (stays in graph)

## Vertex to Edge (ProcessorQuery → LinkQuery)
- out_e() - get outgoing edges
- in_e() - get incoming edges

## Edge Traversals (LinkQuery → ProcessorQuery)
- out_v() - get source vertices
- in_v() - get target vertices

## Edge Narrowing (LinkQuery → LinkQuery)
- first() - narrow to first edge (stays in graph)

## Filters (ProcessorQuery & LinkQuery → same type)
- filter(predicate) - filter by closure on node/edge
- filter(on_component::<C>(predicate)) - filter by component closure
- has(property, value) - filter by property value (searches node fields + components)

## Terminals - Exit Graph (return data, cannot chain)

### ProcessorQuery Terminals
- value() → Option<&ProcessorNode>
- collect() → Vec<&ProcessorNode>
- count() → usize
- exists() → bool

### LinkQuery Terminals
- value() → Option<&Link>
- collect() → Vec<&Link>
- count() → usize
- exists() → bool
