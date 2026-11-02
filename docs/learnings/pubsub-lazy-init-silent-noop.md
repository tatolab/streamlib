# A publish before a spawned listener subscribes is lost, causing test hangs

## Symptom

A test that publishes before its spawned listener has subscribed hangs
forever on `handle.join()` with no output. The test thread never exits,
no panic, no timeout — just blocks forever.

```
running 1 test
test <a test that publishes to a listener on a spawned thread> ...
```
(never completes)

## Root cause

`PUBSUB` is an in-process fan-out with no `init()`: a publish is queued
for every live listener subscribed at that moment, with or without a
runtime, and is lost when that listener's queue is full — a drop the
bus counts and logs — or when the listener's receiver has disconnected,
which removes the subscription without counting a drop. `PUBSUB.subscribe()` returns only once its
subscriber is registered, so subscribing and then publishing needs no
wait in between.

A listener that subscribes from a spawned thread is the exception:
`thread::spawn` returns before the thread reaches its `subscribe()`
call. Combined with the common pattern of `thread::spawn(|| subscribe(...))`
+ `publish(event)` + `handle.join()`, this creates an infinite hang:
- The publish runs before the thread has subscribed — the event is
  queued for nobody
- The listener then waits for an event that already went by
- `join()` blocks forever waiting for the thread to exit

There are zero error messages or warnings. The test looks correct. The
hang is the only symptom.

## Fix (both parts)

1. **Wait for the spawned thread to reach its `subscribe()` call** — on an
   observable effect of the thread having got there, never on a duration.
   A listener loop that subscribes before its first iteration is past its
   `subscribe()` once that first iteration is observed.

2. **Use `mpsc::channel` + `recv_timeout` instead of `handle.join()`**:
```rust
let (done_tx, done_rx) = mpsc::channel();
std::thread::spawn(move || {
    let result = run_the_listener_loop_that_subscribes_first();
    done_tx.send(result).ok();
});

// Publish the event...

match done_rx.recv_timeout(Duration::from_secs(5)) {
    Ok(result) => assert!(result.is_ok()),
    Err(_) => panic!("loop did not exit within 5s after the event was published"),
}
```

## Where this hits

Any test that publishes PUBSUB events (shutdown, reconfigure, etc.) to a
listener that subscribes from a spawned thread.

## Reference
- PUBSUB implementation: `runtime/streamlib-engine/src/core/pubsub/bus.rs`
