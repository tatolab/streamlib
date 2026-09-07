# camera-virtual-camera

One capture device in, **two cameras out**. While this app runs, Chrome, OBS,
Zoom and `v4l2-ctl` all see two extra cameras on the machine — "StreamLib
Camera" showing the capture device untouched, and "StreamLib Camera Inverted"
showing the same picture after a Python processor has inverted its colors. Stop
the app and both are gone.

## The model this example teaches

**A camera is a processor.** `VirtualCameraSink` is a native built-in like
`DisplayWindow`: it takes video on an input port and has no output, because its
output is a camera other applications open. Each instance is one camera that
exists exactly as long as its processor runs — created at `setup()`, removed at
`teardown()` — so from every other application's point of view a StreamLib
graph starting is a USB camera being plugged in, and Ctrl-C is it being pulled
back out.

**As many as the graph adds.** Two cameras is two `rt.add` calls and one more
`rt.connect`; there is no registry, no device number to reserve, and no limit in
the built-in:

```python
def setup(rt: Runtime) -> None:
    camera = rt.add(CameraSource, config=camera_configuration)
    inverting_effect = rt.add(InvertingEffect)
    passthrough_virtual_camera = rt.add(VirtualCameraSink, config={"name": "StreamLib Camera"})
    inverted_virtual_camera = rt.add(VirtualCameraSink, config={"name": "StreamLib Camera Inverted"})

    rt.connect(camera.output("video"), passthrough_virtual_camera.input("video"))
    rt.connect(camera.output("video"), inverting_effect.input("video_from_upstream"))
    rt.connect(inverting_effect.output("video_to_downstream"), inverted_virtual_camera.input("video"))
```

The camera's one output port feeds two consumers. That is an ordinary fan-out —
the passthrough sink and the effect each get every frame — and it is why this
app is two cameras rather than the same camera twice.

**One door per instance, chosen for you.** A virtual camera has to be a device
the rest of the system already knows how to open, and Linux has two such doors:

- the **v4l2loopback** door, where the sink creates its own `/dev/videoN`
  through the module's control node and removes it at teardown. This is the
  door *every* application sees — `/dev/video*` readers directly, and
  portal-based readers through the session manager's V4L2 mirror.
- the **PipeWire** door, a `Video/Source` node with `media.role = Camera`. It
  needs no kernel module and no root, and portal-based applications list it;
  applications that only enumerate `/dev/video*` do not.

The sink picks at `setup()` and logs which and why. Under the default
`door = "auto"` it takes the loopback door whenever the module's control node
is writable by your user, and the PipeWire door otherwise — so a fresh install
with no setup at all still produces a camera. Never both for one instance: the
session manager mirrors V4L2 devices into the portal's camera set, so a sink on
both doors would list the same camera twice.

**The effect publishes its own textures.** `processors/inverting_effect.py`
copies the camera's pixels out, inverts them on the host, and writes them into a
slot of its own `ProcessorOutputTextureRing` — rather than editing the frame in
place the way the effect `streamlib new` scaffolds does. It has to: the
passthrough sink is reading that same engine-owned surface, and an edit in place
would land in the picture it is showing. Whenever a producer's output feeds more
than one consumer, an effect between them owns its output or it corrupts its
sibling.

The two sinks' per-frame paths never enter a Python interpreter — they are
native built-ins, and the RGBA→YUYV conversion is one GPU pass straight into the
device's mapped buffers. The effect runs in its own child interpreter, which is
where the per-frame Python cost in this app begins and ends — and you can see it
in the frame counts: over one 1080p run the passthrough camera wrote 1325 frames
while the inverted one wrote 523 — and both sinks' dropped-frame counters read
zero, so neither was outrun. The effect is simply slower, because a 1920×1080
frame goes out to the host and back for it.
An effect that stays on the GPU — a compute kernel, or a graphics pass as
`examples/camera-python-effects` writes them — does not pay that, and the two
cameras keep pace.

## Run it

```bash
uv venv --python 3.12 && uv sync
streamlib dev
```

`uv sync` installs `streamlib` from the simple index pinned in
`pyproject.toml`; the wheel carries the Python API, the engine, and the
`streamlib` CLI.

That is enough for a camera. If the application you want to open it in
enumerates `/dev/video*` — OBS, `ffmpeg`, `v4l2-ctl`, Chrome as it ships today —
you want the loopback door, and it needs one permission this machine may not
have yet:

```bash
streamlib enable-virtual-camera
```

A one-time step, and the only one that asks for a password: it installs the
standard udev grant (the module loaded with no devices, its control node tagged
`uaccess` for the logged-in seat) through your desktop's own polkit dialog, or
`sudo` in a headless shell. **The engine never runs it, never loads a module and
never asks for elevation** — running it is yours. `--print` writes the three
files and the root commands to stdout instead, for placing them by hand. Where
the grant is missing, `door = "auto"` takes the PipeWire door and says so in the
log, naming this verb.

To work against a checkout rather than a release, install that checkout's wheel
into this venv instead:

```bash
uv pip install maturin
maturin develop --manifest-path ../../sdk/streamlib-python-wheel/Cargo.toml
```

## Observing it

The cameras are the output, so the first place to look is another application.
Both appear by name in any picker: Chrome's `chrome://settings/content/camera`
or a video call's device list, OBS's "Video Capture Device" source, GNOME's
Camera app.

From a terminal, which door the sinks took decides what can see them — the log
line at setup says which it was. **On the v4l2loopback door** they are ordinary
video nodes:

```bash
v4l2-ctl --list-devices        # both by name, beside the real capture devices
ffplay /dev/videoN             # whichever number the list gave you
```

**On the PipeWire door** there is no `/dev/video*` entry to find, so those two
commands will not show them. Ask PipeWire instead:

```bash
pw-dump | grep -A2 node.description   # both by name, among the graph's nodes
pw-cli ls Node                        # the same, node by node
```

Either way, run the listing again after Ctrl-C and neither camera is there.

The running app is also a node, and the usual verbs work from another terminal:

```bash
streamlib nodes           # the live nodes on this machine
streamlib graph           # this node's processors, ports and links, as JSON
streamlib logs --follow   # the node's JSONL log
```

`streamlib logs` is where the door decision is written down — one line per sink
at setup, carrying `door`, the device it created, the module version, and the
reason it chose that door. It also reports which tier the GPU took to reach the
device's buffers (`imported_host_pointer`, or host-cached staging where the
driver refuses the import), and a progress line every 300 frames with the
dropped-frame counts.

To watch the effect's frames rather than its picture, tap its output. Read the
channel name out of `graph` rather than spelling it by hand — a channel is the
source processor's **id**, lowercased, joined to its output port:

```bash
CHANNEL=$(streamlib graph | python3 -c '
import json, sys
for link in json.load(sys.stdin)["links"]:
    source = link["source"]
    if source["port_name"] == "video_to_downstream":
        print((source["processor_id"] + "/" + source["port_name"]).lower())
        break
')
streamlib tap "$CHANNEL" --count 5
```

A video bag is metadata — `surface_id`, `width`, `height`, `timestamp_ns` —
naming pixels that stay in engine-owned GPU memory, so it is a few hundred bytes
whatever the resolution. The `surface_id` differs from the camera's: it names
the effect's own ring slot, which is the whole point of the ring. The
`timestamp_ns` does not — the effect carries the camera's stamp through, so both
cameras present the same picture under the same time.

## Runtime knobs

| Env var | Effect |
| --- | --- |
| `STREAMLIB_PASSTHROUGH_CAMERA_NAME` | The untouched camera's name in every picker. Default `StreamLib Camera`. |
| `STREAMLIB_INVERTED_CAMERA_NAME` | The inverted camera's name. Default `StreamLib Camera Inverted`. |
| `STREAMLIB_VIRTUAL_CAMERA_DOOR` | `auto` (the sink's own default), `v4l2loopback`, or `pipewire`. Applies to both sinks. |
| `STREAMLIB_CAMERA_DEVICE` | Capture device (e.g. `/dev/video0`). Unset picks the first device found. |

A name is what every picker shows, on either door. The loopback device's label
field holds 31 bytes, so a longer name is cut to fit — pick one that reads whole
in a list. Leaving either variable unset gives that row's default, above; this
app always passes a `name`, so the sink's own unnamed fallback — `StreamLib
Camera` plus a short id derived from the app's directory and the processor's
name — is what a graph that omits the key gets, not what you see here.

Naming `v4l2loopback` explicitly turns the fallback off: without the permission
the sink refuses at `setup()` by name, saying it cannot create a camera and
naming `streamlib enable-virtual-camera`. That processor never reaches
`Running`, the rest of the graph keeps going, and `streamlib graph` shows it in
state `Error` — which is what you want when you meant the door that every
application can see and would rather know than be quietly given the other one.
