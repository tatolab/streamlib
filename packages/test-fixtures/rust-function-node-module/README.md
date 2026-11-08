# rust-function-node-fixture

A fixture module for the integration suite: two function nodes written in Rust
with `tatolab-module-sdk`'s `#[function_node]` — `ScaleAndOffset`, stateless,
and `CountsItsOwnCalls`, stateful with a config — built as a maturin wheel the
way a third party builds a module. Each is declared in the Python half with
`node.rust_function(...)`, so it reaches the catalog as `function` / `rust`.
