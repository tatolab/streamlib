# microphone-reverb-speaker

Microphone → reverb → speaker. You hear yourself in a room that isn't there.

> **Wear headphones.** A microphone and a speaker in one room is a feedback loop,
> and a reverb in the middle of it is a feedback loop with gain.

## The model this example teaches

**Two format conversions, neither of them in your code.** Nothing in the graph
names a sample rate, a channel count or a block size. The capture device opens
at whatever it opens at, the playback device likewise, and the only format
anybody states is the one the reverb wants:

| Stage | Format | Who decided it |
| --- | --- | --- |
| Capture device | 48 kHz **stereo**, device-sized blocks | the machine |
| `ReverbEffect`'s input port | 48 kHz **mono** f32, 128-sample windows | the declaration below |
| `SpeakerSink`'s input port | 48 kHz **stereo**, 480-sample windows | the machine's rate, framed by the sink |

The rate and channel count in the last row come off the playback stream;
the 480 is `SpeakerSink`'s own 10 ms window, which is what it settles
`match_device` to where the backend has not told it a period. Both numbers vary
by machine, and neither appears in this stream.

```python
REVERB_WINDOW = AudioWindowContract(
    sample_rate=48_000,
    channels=1,
    dtype="f32",
    window_size=128,
)


dry_audio_from_upstream = Input(Channels, delivery="ordered", window=REVERB_WINDOW)
```

On the way in, the engine mixes the capture device's stereo down to mono and
reframes it into exactly-128-sample windows. On the way out, `SpeakerSink`'s own
input declares `match_device`, so the same stage duplicates the reverb's mono to
stereo and reframes it into 480-sample windows. Both conversions are engine
stages that run before anyone's `process()` does.

`tatolab graph --stream main` reports each input port as an object carrying its
`audio_window`, and the `resolved_from` field is where the two halves show up —
`"declaration"` on the reverb's port, `"device"` on the speaker's:

```json
{
  "name": "dry_audio_from_upstream", "delivery_profile": "ordered",
  "audio_window": {"resolved_from": "declaration", "sample_rate": 48000,
                   "channels": 1, "dtype": "f32", "window_size": 128, "hop": 128}
}
{
  "name": "audio", "delivery_profile": "ordered",
  "audio_window": {"resolved_from": "device", "sample_rate": 48000,
                   "channels": 2, "dtype": "f32", "window_size": 480, "hop": 480}
}
```

That is the whole reason a delay-line algorithm can be a plain Python class
here. `ReverbEffect` reads `ctx.inputs.read(self.dry_audio_from_upstream)`, takes
`numpy.from_dlpack(block)[0]` and knows it has 128 mono samples
at 48 kHz, always, on every machine — so its filter lengths are fixed once in
`__init__` and there is no resampler, no rechunker and no format negotiation
anywhere in `nodes/reverb_effect.py`. What it writes back is a `[1][128]`
array on an `Output(Channels, rate=48_000, dtype="f32")`, with the dry block's
own timestamp and `start_sample`, so the reverberated stream keeps the
microphone's sample index.

Three more things it teaches:

- **A declared contract requires `delivery="ordered"`.** A reverb is an
  accumulator: its output depends on every sample before it. `newest` skips
  messages by design, and a skipped block is a hole in a delay line. `@node.process`
  refuses a contract on a `newest` port at import.
- **The window size is the latency dial** — 128 samples is 2.67 ms at 48 kHz,
  and it is the only number here you would expect to tune. It also has a real
  ceiling: every delay line must be at least one window long, or its ring
  positions alias and the vectorised pass reads samples that should have fed
  back inside the window. The shortest filter here is 245 samples, so that is
  the ceiling. `ReverbDelayLine` refuses rather than quietly computing the
  wrong thing — which is the failure mode that matters, because aliasing
  produces plausible audio rather than an error.
- **The engine flushes rather than interpolating.** If a block is lost, the
  windowing stage drops what it was accumulating and re-anchors on the next
  device stamp instead of blending audio across the gap. The reverb's own delay
  lines carry their tail across it, which is what you would want and is also why
  the gap stays audible rather than being papered over.

`ReverbEffect` runs in its own child interpreter with its own GIL, which imports
it as `nodes.reverb_effect:ReverbEffect` — that is why it lives in its own module
under `nodes/`. `MicrophoneSource` and `SpeakerSink` come from the
`tatolab-microphone` and `tatolab-speaker` modules, and their per-sample paths are
the modules' Rust, never Python.

## Run it

With this machine's runtime running (`tatolabd`, which Tato Studio starts):

```bash
tatolab dev
```

Speak into your microphone and you should hear yourself, wet.

The runtime makes the project's `.venv` on its first load: it installs `tatolab-stream` and `numpy` from PyPI — `tatolab-stream` is
the authoring surface — and builds `tatolab-microphone` and `tatolab-speaker` from this checkout, as editable path dependencies
(`[tool.uv.sources]` in `pyproject.toml`); each module is a maturin project,
so the build needs a Rust toolchain. The
runtime itself is not on pip.

`tatolab dev` follows the project's log and loads the project again on every
saved edit; `tatolab start` queues the load and returns, and `tatolab reload` re-reads a project that already runs. Ctrl-C ends `tatolab dev`
and leaves the stream loaded; `tatolab stop` stops it.

**No audio hardware?** It still runs. The backend chain demotes to a null
backend under which `MicrophoneSource` publishes silence and `SpeakerSink`
discards what it receives — so the graph, both contracts and the reverb are all
exercised in a headless container, silently.

## Observing it

From another terminal, in this folder:

```bash
tatolab streams                  # the streams this machine's runtime holds, `main` among them
tatolab graph --stream <stream>  # nodes, ports, links and counters, as JSON
tatolab logs -f                  # the project's records, Rust and Python alike
```

The nodes are `microphonesource`, `reverbeffect` and `speakersink`.

### Recording the loop instead of shouting at it

A null sink gives you a microphone with no microphone: play a signal into it and
capture it back off its monitor, which is what `<sink>.monitor` device ids are
for on PipeWire. Both nodes take a `device_id`, so the whole loop can be
closed on a machine with no audio hardware attached. Set `microphonesource`'s
`device_id` to `my-null-sink.monitor` and `speakersink`'s to
`my-other-null-sink`.

## Knobs

Each knob is a node's config in `streams/main.graph.json`. Change it in Tato
Studio's inspector or with the runtime's `set_node_config` MCP tool, which
records the edit in the project's history and applies it to the running stream.

| Knob | Node | Effect |
| --- | --- | --- |
| `device_id` | `microphonesource` | Capture device, as the audio backend names it (a CoreAudio device UID on macOS). Absent picks the default device. |
| `device_id` | `speakersink` | Playback device, likewise. Absent picks the default device. |
| `room_size` | `reverbeffect` | How long the tail rings (default 0.7). |
| `damping` | `reverbeffect` | How fast the top end dies inside that tail (default 0.5). |
| `wet_level` | `reverbeffect` | How much of the tail you hear (default 0.25). |
| `dry_level` | `reverbeffect` | How much of the original you hear (default 0.7). |

`REVERB_WINDOW`, at the top of `nodes/reverb_effect.py`, is the latency dial: at
most 245 samples, the shortest delay line.
