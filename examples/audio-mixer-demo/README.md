# audio-mixer-demo

Three sine voices → mixer → speaker. Plays a C major chord.

## The model this example teaches

**A window contract is how a consumer states the audio shape it wants.** The
three voices are deliberately mismatched — different sample rates, different
block sizes, one of them stereo:

| Voice | Frequency | Publishes |
| --- | --- | --- |
| C4 | 261.63 Hz | 48 kHz mono, 512-sample blocks |
| E4 | 329.63 Hz | 44.1 kHz mono, 441-sample blocks |
| G4 | 392.00 Hz | 16 kHz **stereo**, 256-sample blocks |

The mixer declares one contract on all three of its input ports:

```python
CHORD_MIX_WINDOW = AudioWindowContract(
    sample_rate=48_000, channels=1, dtype="f32", window_size=512,
)

@input(delivery_profile="ordered", audio_window=CHORD_MIX_WINDOW)
def root_voice_from_upstream(self) -> None: ...
```

so the engine resamples each voice to 48 kHz, averages the stereo one down to
mono, and frames all three into exactly-512-sample blocks **before** `process()`
runs. What the mixer receives is three windows of identical shape, and mixing
them is one `+`. There is no resampler, no ring buffer and no format negotiation
in this app, because none of that is a user processor's job.

Three further things it teaches:

- **A declared contract requires `delivery_profile="ordered"`.** Order matters
  for samples, and `newest` skips bags by design — an accumulator that needs
  contiguous samples would flush on nearly every read.
- **`SpeakerSink` matches its own device.** The mixer publishes 48 kHz mono; the
  speaker's input declares `match_device`, so the same stage converts the mix to
  whatever the machine's playback device actually opened at. Nothing in `app.py`
  names a device format.
- **Every timestamp is the machine's monotonic clock.** Each voice stamps a
  block with its run anchor plus an integer sample offset, never an accumulated
  per-block delta — which is what keeps 44.1 kHz-family rates exact.
- **The mixer joins on those timestamps, not on arrival order.** Each voice
  starts when its own child interpreter does, so the three streams sit on grids
  tens of milliseconds apart. `ChordMixer` discards any window more than half a
  window behind the newest of the three, which is the block-level join the
  monotonic clock exists for; pairing by arrival order instead would freeze the
  startup skew in for the whole run and publish a timestamp two of the three
  contributions never came from.

Each of the four Python processors runs in its own child interpreter with its own
GIL; `SpeakerSink` is a native built-in inside the wheel.

## Run it

```bash
uv venv --python 3.12 && uv sync
streamlib run
```

You should hear a steady C major chord. Ctrl-C stops it.

`uv sync` installs `streamlib` from the simple index pinned in `pyproject.toml`;
the wheel carries the Python API, the engine and the `streamlib` CLI.

To work against a checkout rather than a release, install that checkout's wheel
into this venv instead:

```bash
uv pip install maturin
maturin develop --manifest-path ../../sdk/streamlib-python-wheel/Cargo.toml
```

**No audio hardware?** It still runs. The backend chain demotes to a null
backend under which `SpeakerSink` discards what it receives — so the graph, the
contract and the mixing are all exercised in a headless container, silently.

## Observing it

From another terminal, while it runs:

```bash
streamlib nodes           # the live nodes on this machine
streamlib graph           # processors, ports, links and per-link drop counts
streamlib logs --follow   # the node's JSONL log
```

`graph` renders each windowed port's contract beside its delivery profile, and
the speaker's `match_device` port renders the five values its device settled on.

To see the mix itself, tap the mixer's output channel. The channel name is the
source processor's id, lowercased, joined to its output port — read it out of
`graph` rather than spelling it by hand:

```bash
CHANNEL=$(streamlib graph | python3 -c '
import json, sys
for link in json.load(sys.stdin)["links"]:
    source = link["source"]
    if source["port_name"] == "mixed_chord_to_downstream":
        print((source["processor_id"] + "/" + source["port_name"]).lower())
        break')
streamlib tap "$CHANNEL" --count 5
```

Unlike a video bag, an audio bag carries its payload: `samples` is the block's
interleaved little-endian bytes, with `sample_rate`, `channels`, `sample_count`,
`dtype` and `first_sample_timestamp_ns` beside them. Audio touches no surface
machinery at all — there is nothing to exchange, because the samples are already
there.

### A handful of dropped windows at startup is expected

The four Python processors are four child interpreters, and they do not finish
spawning at the same instant. A voice that starts first has nowhere to put its
windows until its partners arrive, so the mixer discards its oldest — bounded,
counted, and reported once at teardown:

```
voices ran far enough apart that windows were dropped waiting to be mixed dropped_windows=7
```

It is a startup transient, not a leak: the count stops climbing once all three
voices are live, so a longer run reports roughly the same handful as a short one.
The same startup skew is why the mixer also reports a few `realigned_windows` —
the windows it discarded to bring the three voices onto one instant. Both counts
settle once the grids line up and stay put, because the voices then advance in
lockstep.

## Runtime knobs

Everything is literal config in `app.py` — edit it and re-run. `ToneSource`
takes `frequency_hz`, `sample_rate`, `channels`, `block_size` and `amplitude`;
`ChordMixer` takes `voice_gain`; `SpeakerSink` takes `device_id` (unset picks the
default device — a device that *is* named and cannot be opened raises at
`setup()` rather than silently landing on another one).

Give the three voices the same rate and block size and the contract converts
nothing — the mixer's code does not change, which is the point.
