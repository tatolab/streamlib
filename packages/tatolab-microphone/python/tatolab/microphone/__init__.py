# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The microphone node: `from tatolab.microphone import MicrophoneSource`.

The node runs in its own helper process like any Python node. Its native
half opens the capture stream through the engine's audio device backend, which
the module host table exports, so PipeWire, ALSA and CoreAudio are the
engine's own arms and no second audio device path exists. The device callback
hands each block to a native thread that publishes it on `audio` with no
Python attached: Python runs only when an input changes, never per block.
"""

from __future__ import annotations

from typing import Annotated

from tatolab.stream import (
    Audio,
    Description,
    Input,
    OutputPin,
    PinControl,
    RuntimeContextFullAccess,
    computed,
    log,
    node,
)

from . import _native

__all__ = ["AUDIO_OUTPUT_PORT", "MicrophoneSource"]

AUDIO_OUTPUT_PORT = "audio"


@node.meta(
    description=(
        "Captures audio from the machine's audio backend as timestamped sample blocks "
        "(silence where no backend exists)."
    ),
)
@node(execution="manual", scheduling="realtime", paced_by_real_time=True)
def MicrophoneSource(
    *,
    device_id: Input[
        Annotated[
            str | None,
            PinControl("audio_capture_device"),
            Description(
                "Backend-named capture device, as the runtime's `list_audio_capture_devices` "
                "lists it. Unset or empty: the backend's default device. A name the backend "
                "cannot open fails the open rather than landing on a different device — a wrong "
                "device id is a wiring error. On the PipeWire backend, `<sink>.monitor` captures "
                "what that sink is playing, and `@DEFAULT_MONITOR@` what the default output is "
                "playing when the device opens."
            ),
        ]
    ] = Input(None),
    audio: OutputPin[
        Annotated[Audio, Description("Timestamped planar blocks of f32 capture samples")]
    ],
    ctx: RuntimeContextFullAccess,
) -> None:
    """Captures audio from the machine's audio backend as timestamped sample blocks
    (silence where no backend exists).

    The engine's backend chain is probed in this node's helper process with no
    configuration dial; where no audio backend exists at all the blocks are
    silence, so a pipeline authored on a workstation runs unchanged in a headless
    container. Blocks arrive on `audio` as f32 Channels blocks at the device's
    rate and channel count, an i16 device widened to f32, each stamped with the
    device's instant of its first sample.

    The open capture stream is a computed value: setting `device_id` while the
    stream plays opens the new device while the old one keeps publishing, then
    closes the old one; a device that fails to open shows on the node, and the
    old one keeps publishing.

    On macOS the backend is CoreAudio and `device_id` is a CoreAudio device UID;
    naming one that is not attached is refused with the UIDs that are. Only a
    stream that holds this node opens a microphone, so only such a stream can
    put the platform's access prompt in front of the user, and it asks only when
    the stream's load allows: a stream the runtime re-loads on its own never
    asks, and its microphone fails to open until access has been answered.
    """
    del audio  # Lent to the native half, which publishes on it.
    module_host_table = ctx.open_module_host_table(output_ports=[AUDIO_OUTPUT_PORT])

    def open_the_microphone() -> _native.MicrophoneCapture:
        named = device_id.get() or None
        capture = _native.MicrophoneCapture(module_host_table, named)
        log.info(
            f"MicrophoneSource: capture stream opened on {named or '<default>'} at "
            f"{capture.sample_rate} Hz × {capture.channels} ch, {capture.sample_format}"
        )
        capture.start()
        return capture

    computed(open_the_microphone)
