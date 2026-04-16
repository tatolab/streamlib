# PUBSUB silently no-ops without init(), causing test hangs

## Symptom

A test that uses `PUBSUB.subscribe()` + `PUBSUB.publish()` hangs
indefinitely with no error output. The test thread never exits, no
panic, no timeout — just blocks forever on `handle.join()`.

```
running 1 test
test core::utils::loop_control::tests::test_shutdown_event_exits_loop ...
```
(never completes)

## Root cause

`PUBSUB` is a `LazyLock<PubSub>` that uses `OnceLock` for its internal
`runtime_id` and iceoryx2 `node`. It is only fully functional after
`PUBSUB.init("name", node)` is called — which happens inside
`StreamRuntime::new()`.

Without `init()`:
- `subscribe()` **buffers the subscription** (does not fail)
- `publish()` **silently drops the event** (does not fail)

Combined with the common pattern of `thread::spawn(|| subscribe(...))` +
`publish(event)` + `handle.join()`, this creates an infinite hang:
- The subscriber thread opens an iceoryx2 service and waits for events
- The publish drops silently — the event never arrives
- `join()` blocks forever waiting for the thread to exit

There are zero error messages or warnings. The test looks correct. The
hang is the only symptom.

## Compound failure (iceoryx2 interaction)

Even with PUBSUB initialized, a second failure mode exists: if the
iceoryx2 service is in `ServiceInCorruptedState` (from parallel test
teardown), the subscriber thread silently exits without receiving the
event. The event is published to... nothing. `join()` may complete
(thread exited) but the expected event was never received.

## Fix (all three parts)

1. **Initialize PUBSUB in the test** if a `StreamRuntime` isn't being created:
```rust
if let Ok(node) = Iceoryx2Node::new() {
    PUBSUB.init("test-name", node);
}
```

2. **Use `mpsc::channel` + `recv_timeout` instead of `handle.join()`**:
```rust
let (done_tx, done_rx) = mpsc::channel();
std::thread::spawn(move || {
    let result = shutdown_aware_loop(|| { ... });
    done_tx.send(result).ok();
});

// Publish the event...

match done_rx.recv_timeout(Duration::from_secs(5)) {
    Ok(result) => assert!(result.is_ok()),
    Err(_) => panic!("loop did not exit within 5s — PUBSUB may not be initialized"),
}
```

3. **Allow setup time** — `std::thread::sleep(Duration::from_millis(150))`
   between spawning the subscriber thread and publishing. The iceoryx2
   service open is async; publishing before the subscriber is listening
   loses the event.

## Where this hits

Any test that uses PUBSUB events (shutdown, reconfigure, etc.) outside
of a full `StreamRuntime`. Currently:
- `libs/streamlib/src/core/utils/loop_control.rs` — `test_shutdown_event_exits_loop`

## Reference
- Fix commit in #252 (ash → vulkanalia migration branch)
- PUBSUB implementation: `libs/streamlib/src/core/pubsub.rs`
- iceoryx2 node: `libs/streamlib/src/iceoryx2/mod.rs`
