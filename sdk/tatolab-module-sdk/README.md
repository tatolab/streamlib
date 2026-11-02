# tatolab-module-sdk

The module host table a Tatolab module's native code reaches its node's ports
through, and safe Rust over it. It depends on no engine crate: a module builds
against this crate alone, and the runtime that starts its node lends the table.

## What the table carries

Version 1.0 carries, for the ports a node lends to its native code:

- open, write and catalog-entry on an output port;
- open, read and wait on input ports;
- the node's stop flag;
- log records into the node's log, at the host's levels;
- the runtime's monotonic clock;
- the host's capabilities, as JSON.

Every verb is callable from any thread with no Python attached. The table carries
plain data and opaque handles only. No engine or GPU object crosses it, and
`cargo xtask check-module-host-table-surface` keeps it that way.

## Reaching it

A node's Python half lends ports in `start()` and passes the capsule it gets
back to its native half:

```python
module_host_table = ctx.open_module_host_table(
    output_ports=["encoded_video", "encoded_audio"]
)
self._thread = _native.MyReadingThread(module_host_table)
```

Once a port is lent, a write to it from Python is refused, naming the port. To
lend an input port, the node must be declared `execution="manual"`, so that the
runtime does not also drive `process()` from it.

The native half reads the capsule with the `pyo3` feature, then hands the
`ModuleHost` to whichever thread does the work:

```rust
let module_host = ModuleHost::from_python_module_host_table_capsule(&module_host_table)?;
let encoded_video = module_host.open_output_port("encoded_video")?;
std::thread::spawn(move || {
    while !module_host.stop_requested() {
        if encoded_video.write(&payload, None, true).is_err() {
            break;
        }
    }
});
```

With the default `tracing` feature,
`module_host_tracing::forward_this_modules_tracing_records_to(&module_host)`
routes the module's own `tracing` records into the node's log.

## Versioning

The table is append-only. A module built against minor N loads on any runtime
that lends minor N or later. Loading on an older minor, or on a different
major, is refused with an error that names both versions. This crate's
`major.minor` is the table's own; a patch release never changes the table.
