# microphone-reverb-speaker

Microphone → reverb → speaker. You hear yourself in a room that isn't there.

> **Wear headphones.** A microphone and a speaker in one room is a feedback loop,
> and a reverb in the middle of it is a feedback loop with gain.

## The model this example teaches

**Two format conversions, neither of them in your code.** Nothing in `app.py`
names a sample rate, a channel count or a block size. The capture device opens
at whatever it opens at, the playback device likewise, and the only format
anybody states is the one the reverb wants:

| Stage | Format | Who decided it |
| --- | --- | --- |
| Capture device | 48 kHz **stereo**, device-sized blocks | the machine |
| `ReverbEffect`'s input port | 48 kHz **mono** f32, 128-sample windows | the declaration below |
| Playback device | 48 kHz **stereo**, 480-sample periods | the machine |

```python
REVERB_WINDOW = AudioWindowContract(
    sample_rate=48_000, channels=1, dtype="f32", window_size=128,
)

@input(delivery_profile="ordered", audio_window=REVERB_WINDOW)
def dry_audio_from_upstream(self) -> None: ...
```

On the way in, the engine mixes the capture device's stereo down to mono and
reframes it into exactly-128-sample windows. On the way out, `SpeakerSink`'s own
input declares `match_device`, so the same stage duplicates the reverb's mono to
stereo and reframes it into the device's 480-sample periods. Both conversions
are engine stages that run before anyone's `process()` does. Run it and look:

```
"dry_audio_from_upstream": {"resolved_from": "declaration", "channels": 1, "window_size": 128}
"audio":                   {"resolved_from": "device",      "channels": 2, "window_size": 480}
```

That is the whole reason a delay-line algorithm can be a plain Python class
here. `ReverbEffect` reads `block.samples[:, 0]` and knows it has 128 mono
samples at 48 kHz, always, on every machine — so its filter lengths are fixed
once in `__init__` and there is no resampler, no rechunker and no format
negotiation anywhere in the file.

Three more things it teaches:

- **A declared contract requires `delivery_profile="ordered"`.** A reverb is an
  accumulator: its output depends on every sample before it. `newest` skips bags
  by design, and a skipped block is a hole in a delay line.
- **The window size is the latency dial** — 128 samples is 2.67 ms at 48 kHz,
  and it is the only number here you would expect to tune. It also has a real
  ceiling: every delay line must be longer than one window, or a window feeds
  back into itself inside a single vectorised pass. The shortest filter here is
  245 samples, so that is the ceiling. `ReverbDelayLine` refuses rather than
  quietly computing the wrong thing.
- **The engine flushes rather than interpolating.** If a block is lost, the
  windowing stage drops what it was accumulating and re-anchors on the next
  device stamp instead of blending audio across the gap. The reverb's own delay
  lines carry their tail across it, which is what you would want and is also why
  the gap stays audible rather than being papered over.

`ReverbEffect` runs in its own child interpreter with its own GIL;
`MicrophoneSource` and `SpeakerSink` are native built-ins inside the wheel, and
their per-sample paths never enter an interpreter at all.

## Run it

```bash
uv venv --python 3.12 && uv sync
streamlib run
```

Speak into your microphone and you should hear yourself, wet. Ctrl-C stops it.

`uv sync` installs `streamlib` from the simple index pinned in `pyproject.toml`;
the wheel carries the Python API, the engine and the `streamlib` CLI.

To work against a checkout rather than a release, install that checkout's wheel
into this venv instead:

```bash
uv pip install maturin
maturin develop --manifest-path ../../sdk/streamlib-python-wheel/Cargo.toml
```

**No audio hardware?** It still runs. The backend chain demotes to a null
backend under which `MicrophoneSource` publishes silence and `SpeakerSink`
discards what it receives — so the graph, both contracts and the reverb are all
exercised in a headless container, silently.

## Observing it

From another terminal, while it runs:

```bash
streamlib nodes           # the live nodes on this machine
streamlib graph           # processors, ports, links and per-link drop counts
streamlib logs --follow   # the node's JSONL log
```

`graph` renders each windowed port's contract beside its delivery profile, which
is where the two rows above come from.

To see the wet signal itself, tap the reverb's output channel. The channel name
is the source processor's id, lowercased, joined to its output port — read it
out of `graph` rather than spelling it by hand:

```bash
CHANNEL=$(streamlib graph | python3 -c '
import json, sys
for link in json.load(sys.stdin)["links"]:
    source = link["source"]
    if source["port_name"] == "reverberated_audio_to_downstream":
        print((source["processor_id"] + "/" + source["port_name"]).lower())
        break')
streamlib tap "$CHANNEL" --count 20
```

Unlike a video bag, an audio bag carries its payload: `samples` is the block's
interleaved little-endian bytes, with `sample_rate`, `channels`, `sample_count`,
`dtype` and `first_sample_timestamp_ns` beside them. Every bag off this port is
512 bytes — 128 mono `f32` samples — and consecutive stamps are one window
apart: 2 666 666 or 2 666 667 ns, alternating, because 128/48 000 s is not a
whole number of nanoseconds and the stamps are derived in integer arithmetic
from one anchor rather than accumulated a rounded delta at a time.

### Recording the loop instead of shouting at it

A null sink gives you a microphone with no microphone: play a signal into it and
capture it back off its monitor, which is what `<sink>.monitor` device ids are
for. Both built-ins take a `device_id`, so the whole loop can be closed on a
machine with no audio hardware attached:

```python
rt.add(MicrophoneSource, config={"device_id": "my-null-sink.monitor"})
rt.add(SpeakerSink, config={"device_id": "my-other-null-sink"})
```

## Runtime knobs

Everything is literal config in `app.py` — edit it and re-run. `ReverbEffect`
takes four 0..1 dials:

| Dial | Default | What it does |
| --- | --- | --- |
| `room_size` | 0.7 | how long the tail rings |
| `damping` | 0.5 | how fast the top end dies inside that tail |
| `wet_level` | 0.25 | how much of the tail you hear |
| `dry_level` | 0.7 | how much of the original you hear |

The two levels are set so the defaults cannot clip: the wet path's own gain
peaks around 1.15×, so 0.7 + 0.25 × 1.15 = 0.99, just under full scale. Raise
them and the clamp in `process()` is what stops a speaker from being asked for
something it cannot play.

`MicrophoneSource` and `SpeakerSink` each take a `device_id`; unset picks the
default device, and one that *is* named and cannot be opened raises at `setup()`
rather than silently landing on another.
