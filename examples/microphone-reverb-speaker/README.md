# microphone-reverb-speaker

Microphone → reverb → speaker. You hear yourself in a room that isn't there.

> **Wear headphones.** A microphone and a speaker in one room is a feedback loop,
> and a reverb in the middle of it is a feedback loop with gain.

## The model this example teaches

**Two format conversions, neither of them in your code.** Nothing in `stream.py`
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


@node.input(delivery_profile="ordered", audio_window=REVERB_WINDOW)
def dry_audio_from_upstream(self) -> AudioBlock: ...
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
here. `ReverbEffect` reads `ctx.inputs.read("dry_audio_from_upstream",
into=AudioBlock)`, takes `block.samples[:, 0]` and knows it has 128 mono samples
at 48 kHz, always, on every machine — so its filter lengths are fixed once in
`__init__` and there is no resampler, no rechunker and no format negotiation
anywhere in `nodes/reverb_effect.py`. What it writes back is an `AudioBlock`
too, built and packed with `to_bytes()`, so the record's keys are checked where
it is made rather than discovered wrong downstream.

Three more things it teaches:

- **A declared contract requires `delivery_profile="ordered"`.** A reverb is an
  accumulator: its output depends on every sample before it. `newest` skips
  messages by design, and a skipped block is a hole in a delay line. `@node.input`
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
it as `nodes.reverb_effect:ReverbEffect` — that is why it lives under `nodes/`
and never in `stream.py`. `MicrophoneSource` and `SpeakerSink` are native
built-ins that ship inside the runtime, and their per-sample paths never enter an
interpreter at all.

## Run it

With this machine's runtime running (`tatolabd`, which the installer starts):

```bash
uv sync
tatolab dev
```

Speak into your microphone and you should hear yourself, wet. Ctrl-C unloads the
stream.

`uv sync` installs `tatolab-stream` and `numpy` from PyPI — `tatolab-stream` is
the authoring surface, and the only Tatolab package a stream's venv holds. The
runtime itself is not on pip.

Use `tatolab dev` while editing — it loads the stream again on every saved edit,
so a change to a dial or to the reverb is heard on the next save — and
`tatolab run` otherwise. Both keep the stream as long as the command runs.

**No audio hardware?** It still runs. The backend chain demotes to a null
backend under which `MicrophoneSource` publishes silence and `SpeakerSink`
discards what it receives — so the graph, both contracts and the reverb are all
exercised in a headless container, silently.

## Observing it

From another terminal:

```bash
tatolab streams                 # the streams this machine's runtime holds
tatolab graph --stream main     # nodes, ports, window contracts, links and counters, as JSON
tatolab logs --stream main -f   # the stream's records, Rust and Python alike
```

`graph` renders each windowed port's contract beside its delivery profile, which
is where the two rows above come from. A node is named after its class,
lowercased — `microphonesource`, `reverbeffect`, `speakersink` — unless `add`
passes `name=`.

To see the wet signal itself, expose the reverb's output; every port in this
stream is internal to it until you do. An exposed port is the track
`<node>/<port>` of the broadcast `<machine>/<stream>.hang` at the machine's local
relay — `http://127.0.0.1:4446/` lists this machine's name — so any MoQ client
reads it; this repository's `tatolab-read-track` writes a track's objects to
files:

```bash
tatolab expose main reverbeffect reverberated_audio_to_downstream
cargo run -p tatolab-read-track -- http://127.0.0.1:4443 <machine>/main.hang \
    reverbeffect/reverberated_audio_to_downstream --count 20 --out /tmp/wet
```

Each object is one block's msgpack record, indexed with its timestamp in
`objects.jsonl`. Unlike a video frame's record, an audio block's carries its
payload: `samples` is the block's interleaved little-endian bytes, with
`sample_rate`, `channels`, `sample_count`, `dtype` and
`first_sample_timestamp_ns` beside them. The payload off this port is always 512
bytes — 128 mono `f32` samples — and consecutive stamps are one window apart: 2
666 666, 2 666 667, 2 666 667, repeating, and summing to exactly 8 000 000 ns
every three windows. 128/48 000 s is not a whole number of nanoseconds, so a
stamp that accumulated a rounded delta would drift; each one is instead derived
from a single anchor in integer arithmetic, which is what makes the cycle exact
rather than approximately right. The reverb keeps the stamp each block arrived
with — its tail rides later windows, but these samples cover the instants the
incoming ones did, so A/V sync downstream stays subtraction.

`tatolab expose main reverbeffect reverberated_audio_to_downstream --remove`
makes the port internal again; the microphone's `audio` port is read the same way.

### Recording the loop instead of shouting at it

A null sink gives you a microphone with no microphone: play a signal into it and
capture it back off its monitor, which is what `<sink>.monitor` device ids are
for on PipeWire. Both built-ins take a `device_id`, so the whole loop can be
closed on a machine with no audio hardware attached. Set the two constants at
the top of `stream.py`:

```python
MICROPHONE_DEVICE_ID: str | None = "my-null-sink.monitor"
SPEAKER_DEVICE_ID: str | None = "my-other-null-sink"
```

which the stream passes on as

```python
stream_builder.add(MicrophoneSource, config={"device_id": "my-null-sink.monitor"})
stream_builder.add(SpeakerSink, config={"device_id": "my-other-null-sink"})
```

## Knobs

Everything is literal config in `stream.py` — edit it and save while
`tatolab dev` runs. A stream is compiled in the runtime's environment, not your
shell's, so nothing here is read from an environment variable.

| Knob | Where | Effect |
| --- | --- | --- |
| `MICROPHONE_DEVICE_ID` | top of `stream.py` | Capture device, as the audio backend names it (a CoreAudio device UID on macOS). `None` picks the default device. |
| `SPEAKER_DEVICE_ID` | top of `stream.py` | Playback device, likewise. `None` picks the default device. |
| `room_size` | `ReverbEffect`'s `config` | how long the tail rings (default 0.7) |
| `damping` | `ReverbEffect`'s `config` | how fast the top end dies inside that tail (default 0.5) |
| `wet_level` | `ReverbEffect`'s `config` | how much of the tail you hear (default 0.25) |
| `dry_level` | `ReverbEffect`'s `config` | how much of the original you hear (default 0.7) |
| `REVERB_WINDOW` | top of `nodes/reverb_effect.py` | the latency dial; at most 245 samples, the shortest delay line |

The four dials each run from 0.0 to 1.0; `ReverbEffectConfig` declares them, and
`ReverbEffect` refuses a value outside that range when it is constructed. A
named device that cannot be opened raises rather than silently landing on
another.

The two levels leave headroom for most of what a microphone produces — a
full-scale sine measures 0.64 at 440 Hz, 0.78 at 1 kHz and 0.69 at 8 kHz, none
of them touching the clamp — but there is no bound here worth relying on. The
wet path peaks near **2.5× at 5 766 Hz**, where the eight combs' resonances
line up and the four diffuser stages (which are not allpass, whatever Freeverb
calls them) multiply on top. The resonance is razor-thin — 5 760 Hz and
5 770 Hz both come out under 0.6 — but a full-scale sine sitting on it clamps
41% of its samples, and a full-scale 40 Hz square wave clamps 30%.

That is worth more than a footnote given the warning at the top of this file:
acoustic feedback locks onto whichever frequency the loop has the most gain at,
which is precisely that 5.77 kHz peak. Headphones.
