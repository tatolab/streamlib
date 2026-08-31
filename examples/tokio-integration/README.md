# tokio-integration

A StreamLib graph running inside an existing tokio service. `#[tokio::main]`
owns the process; the engine is a guest in it.

Nothing here is a StreamLib entry point — there is no `streamlib run`, no
`setup(rt)` found by convention, and no scaffold. A Rust app is a plain cargo
project that depends on the `streamlib` crate and calls the engine itself, so
this one is `cargo run`.

## The model this example teaches

**The engine adopts your tokio runtime; it never brings a second one.**

```rust
#[tokio::main]
async fn main() -> Result<()> {
    let engine_runtime = Runner::new()?;   // finds this thread's runtime, takes its handle
```

Called outside a tokio context, `Runner::new()` builds a multi-thread runtime
and owns it. Called inside one — as here — it takes the current handle instead.
Nothing is configured for this; it is the same constructor either way.

### From async code, the graph ops are the `*_async` ones

Every graph op has two spellings, and they are not interchangeable in a task:

```rust
let source_id = engine_runtime.add_processor_async(ProcessorSpec::new(source_class, config)).await?;
engine_runtime.connect_async(from, to).await?;
```

The sync twin (`add_processor`, `connect`) hands the work to the runtime and
then blocks the calling thread until the reply lands. On a multi-thread runtime
that costs a worker for the round trip; on `#[tokio::main(flavor =
"current_thread")]` there is no second thread to run the work, so it never
returns. The `*_async` twins are the ones written for a caller that is already
async.

Two calls are exceptions worth knowing, and this app leans on both:

- **`add_local`** registers an already-compiled `#[processor]` type on the
  registry and returns the class import path. It touches no graph and waits on
  nothing, so async code calls it directly.
- **`request_runtime_shutdown`** is a *request*, not a teardown — it publishes
  and returns without waiting for an answer, which is what makes it the one
  lifecycle op a task can call.

This is also why the app talks to `Runner` directly rather than through `App`,
the authoring sugar a Rust app would otherwise reach for first. `App` is a thin
wrapper — `add`, `add_local`, `connect`, `run` — but every one of its methods is
the sync spelling, and it has no `*_async` twins. In a `fn main()` that is not
async, `App` is the shorter way to write this graph; in a tokio service it is
the wrong one. `App::runner()` is the escape hatch back if you want the sugar
for setup and the async ops for the rest.

### The run loop is blocking, and it keeps the main thread

```rust
// … every observer task already spawned above …
let run_outcome = engine_runtime.start_and_wait_for_shutdown();
```

`start_and_wait_for_shutdown` takes ownership of the shutdown signals, starts
the graph, and then polls until one arrives — a loop with a sleep in it. This
is the one piece of the integration that has to be placed deliberately, and the
placement is: **leave it on the process main thread.**

`#[tokio::main]` drives `main`'s future on the main thread via `block_on`, while
the multi-thread runtime's workers are threads of their own. So blocking here
parks main and starves nothing — every task spawned before this line keeps
running. Spawn your observers first, then call it last.

The tempting alternative, `spawn_blocking(move || …start_and_wait_for_shutdown())`,
works on Linux and **panics on macOS**: there the wait becomes the
`NSApplication` event loop, which asserts it is on the main thread, and a
blocking-pool thread never is. Every other caller in the tree — `App::run`, the
Python wheel's `Runtime.run` — calls it on its caller's thread for the same
reason.

### A tap is how the graph's data reaches async code

```rust
let mut tap = engine_runtime.tap_async(tick_channel, None).await?;
while let Some(tapped_bag) = tap.recv().await { … }
```

`tap_async` is a read-only subscription on one channel, and `recv()` is a
genuine `async fn` — so the graph's bags land in a tokio task with nothing you
have to write: no bridging channel of your own, no static, no poll loop. It is
the async-native seam between the two worlds. (Inside, the engine does own a
thread for the `!Send` subscriber and forwards over a bounded channel; that is
the machinery you are being handed rather than machinery that is absent.)

Three properties that matter when you build on it:

- **The bag is raw wire form.** The engine forwards it verbatim and reads none
  of it, so the reader skips `FRAME_HEADER_SIZE` bytes and decodes the msgpack
  behind it — with `rmp_serde` here, which is an ordinary crates.io dependency
  and not something the engine supplies.
- **A tap drops rather than back-pressures.** A slow consumer loses bags and
  `tap.dropped_bags()` counts them; the producer is never paced by an observer.
- **One tap per channel.** It consumes the channel's single reserved subscriber
  slot, so a second attach is refused until the first detaches.

Dropping the subscription joins its forwarder thread, which is why this app
drops it inside `spawn_blocking` rather than on an async worker.

### The graph is readable from async code too

`to_json_async()` returns the same JSON `streamlib graph` serves an API
consumer — nodes with their ports, components and state, plus the links. This
app reads it twice: to wait for every processor to reach `Running` before
attaching the tap, and to report the graph's state every few seconds while it
runs.

The engine ships that first question as `wait_until_every_processor_is_running`,
and it is the call to reach for first. This app polls instead, for two reasons a
blocking wait cannot give a service: it gives up the moment the engine comes
down, and it leaves nothing behind — a `spawn_blocking` wait that outlives its
budget is still running when the tokio runtime shuts down, and the runtime waits
for it. The trade is not free: `to_json_async` serializes the whole graph under
the graph lock, which is the lock processor threads need in order to publish the
very transitions being waited on. Cheap at two nodes; think again at fifty.

### Both processors are Rust classes in this binary

```rust
#[streamlib::sdk::processor(
    execution = continuous(interval_ms = 40),
    output("tick_to_downstream", description = "Sequenced ticks"),
)]
pub struct SequencedTickSource { next_sequence_number: u64 }
```

A processor is named by the import path of the class it is, captured by the
macro where it expands — so `add_local` needs no manifest, no package on disk
and no build step. The pair here is deliberately dull, because the subject is
the integration and not the pipeline: a continuous source stamps a sequence
number and a `MediaClock::now()` reading onto a bag every 40 ms, and a reactive
sink reports the cadence a window of them actually arrived at, along with any
sequence numbers it never saw.

The sink's input declares `delivery_profile = "ordered"`, which is what makes
those intervals mean anything — `"newest"` would drain to the latest bag on
every wake and the gaps would be the reader's, not the link's.

### What this app does not do

It does not host the control plane, so `streamlib nodes` and `streamlib graph`
cannot see it — those verbs talk to the api-server a node hosts in-process, and
this one hosts nothing. That is a deliberate omission rather than a missing
capability: the point being made here is that the engine is drivable from
async code, and the app reads its own graph through `to_json_async` to show it.

## Run it

```bash
cargo run
```

It runs for 20 seconds and then stops itself, which is the deadline task asking
the graph to shut down from async code. Ctrl-C does the same thing sooner —
the run loop owns the signal, so a single Ctrl-C is a clean shutdown, not a
kill.

Expect the first build to take a while and to be CPU-heavy. This is a
standalone cargo project with its own `target/`, so it shares nothing with the
checkout's build, and the engine carries a vendored C++ GLSL compiler that is
built from source.

It needs a working Vulkan device: `start()` initializes the GPU context before
any processor runs, and fails there rather than degrading if it cannot.

Out of tree, the only line that changes is where the dependency comes from.
`Cargo.toml` here takes a path into the checkout it ships inside; an app outside
the repo takes a git dependency pinned to a tag:

```toml
streamlib = { git = "https://github.com/tato123/streamlib", tag = "v0.18.29" }
```

The plan is for the `streamlib` crate to be released alongside the wheel on one
version, at which point that becomes `streamlib = "0.18"`. No workflow publishes
the crate yet, so a bare version requirement does not resolve today.

## Editing it

Everything worth turning is a constant at the top of a file.

| Knob | Where | What it does |
| --- | --- | --- |
| `SERVICE_RUN_DURATION` | `src/main.rs` | how long before the deadline task asks the graph to stop |
| `GRAPH_REPORT_INTERVAL` | `src/main.rs` | how often async code prints the graph's state |
| `TICKS_PER_CADENCE_REPORT` | `src/main.rs` | ticks the in-graph sink gathers before it reports; below 2 the sink refuses at `setup`, because there is no interval to read |
| `interval_ms` | `src/sequenced_tick_source.rs` | the source's cadence, in its `#[processor]` attribute |

Two edits worth making on purpose, because each fails in an instructive way:

- Swap an `add_processor_async` for its sync `add_processor` and change
  `#[tokio::main]` to `#[tokio::main(flavor = "current_thread")]`. The app
  hangs on the first graph op, because the work it is waiting for has no thread
  to run on.
- Change the sink's input to `delivery_profile = "newest"`. The ticks keep
  flowing, but any wake that finds more than one bag queued now collapses them:
  `newest` drains to the latest and discards the rest, so the intervals it does
  measure widen and the ones it skipped inside a window surface as
  `skipped_tick_count`. (Loss falling between two windows is invisible to that
  counter by construction — it is derived from the first and last sequence
  numbers of one window.)

## Testing it

```bash
cargo test
```

The pure parts have unit tests — the cadence arithmetic, and the two helpers
that read the graph's JSON. There is no CI presence: the showcase is kept
current by convention, not by a gate.
