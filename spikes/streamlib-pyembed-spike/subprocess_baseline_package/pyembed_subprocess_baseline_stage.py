# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The #1702 baseline arm: the spike's stage callback, hosted in a subprocess.

Byte-for-byte the same work the in-process arm does per frame — read the
payload, hand a numpy view of the frame's pixels to the same callable from
``spike_stage_callbacks``, patch the stage duration into the measurement
preamble, write the payload on. Everything that differs is the hosting: a
separate process, a per-package venv, and the payload crossing iceoryx2 through
the python-native cdylib rather than staying in one address space.

Two deliberate departures from the SDK's public surface, both in service of
measuring the same quantity as the in-process arm:

* ``inputs._read_raw`` / ``slpn_output_write`` are used directly instead of
  ``inputs.read`` / ``outputs.write``. The public pair msgpack-encodes and
  decodes, which the in-process arm's ``read_raw``/``write_raw`` do not — the
  encode would land inside the measured span and show up as PyO3 winning.
* The pixels the callback views come from a process-local buffer allocated at
  setup, exactly as on the in-process arm. A real subprocess would pay a
  cpu-readback escalate round trip to reach a GPU surface's pixels, so this
  biases the comparison *toward* the baseline. That direction is the safe one:
  a GO decided against a conservatively-fast baseline is still a GO.
"""

from __future__ import annotations

import ctypes
import importlib
import struct
import time

# Mirrors `synthetic_frame_measurement_preamble.rs`: little-endian, packed,
# u64 sequence number, i64 source emit stamp, i64 stage callback duration.
MEASUREMENT_PREAMBLE_BYTES = 24
STAGE_CALLBACK_NANOSECONDS_OFFSET = 16

DEFAULT_STAGE_CALLBACK_MODULE = "spike_stage_callbacks"
DEFAULT_STAGE_CALLBACK_ATTRIBUTE = "passthrough_stage"


class PyembedSubprocessBaselineStage:
    """Reactive stage that runs the spike callback once per frame."""

    def setup(self, ctx) -> None:
        import numpy

        config = ctx.config
        self._frame_width_pixels = int(config.get("frame_width_pixels", 1920))
        self._frame_height_pixels = int(config.get("frame_height_pixels", 1080))
        self._channel_count = int(config.get("channel_count", 4))
        self._wire_payload_mode = str(
            config.get("wire_payload_mode", "surface-reference")
        )

        module_name = str(
            config.get("stage_callback_module", DEFAULT_STAGE_CALLBACK_MODULE)
        )
        attribute_name = str(
            config.get("stage_callback_attribute", DEFAULT_STAGE_CALLBACK_ATTRIBUTE)
        )
        self._stage_callback = getattr(
            importlib.import_module(module_name), attribute_name
        )

        pixel_byte_count = (
            self._frame_width_pixels * self._frame_height_pixels * self._channel_count
        )
        # Same non-uniform fill the in-process arm's locally resolved surface
        # carries, so a callback that inspects content sees identical input.
        pattern = bytes((index % 251) for index in range(251))
        repeats = pixel_byte_count // len(pattern) + 1
        self._locally_resolved_surface_pixels = numpy.frombuffer(
            (pattern * repeats)[:pixel_byte_count], dtype=numpy.uint8
        ).reshape(
            self._frame_height_pixels, self._frame_width_pixels, self._channel_count
        ).copy()

        self._observed_frame_count = 0

    def process(self, ctx) -> None:
        import numpy

        frame_payload, frame_timestamp_ns = ctx.inputs._read_raw("frame_in")
        if frame_payload is None:
            return
        if len(frame_payload) <= MEASUREMENT_PREAMBLE_BYTES:
            raise ValueError(
                f"frame payload of {len(frame_payload)} bytes carries nothing after "
                "the measurement preamble"
            )

        if self._wire_payload_mode == "full-pixel-payload":
            frame_view = numpy.frombuffer(
                frame_payload, dtype=numpy.uint8, offset=MEASUREMENT_PREAMBLE_BYTES
            ).reshape(
                self._frame_height_pixels,
                self._frame_width_pixels,
                self._channel_count,
            )
        else:
            frame_view = self._locally_resolved_surface_pixels

        callback_started_ns = time.monotonic_ns()
        self._stage_callback(frame_view)
        callback_finished_ns = time.monotonic_ns()

        outgoing = bytearray(frame_payload)
        struct.pack_into(
            "<q",
            outgoing,
            STAGE_CALLBACK_NANOSECONDS_OFFSET,
            callback_finished_ns - callback_started_ns,
        )
        _write_raw_payload(ctx, "frame_out", bytes(outgoing), frame_timestamp_ns)
        self._observed_frame_count += 1

    def teardown(self, ctx) -> None:
        from streamlib import log

        log.info(
            "subprocess baseline stage complete",
            observed_frame_count=self._observed_frame_count,
        )


def _write_raw_payload(ctx, port_name: str, payload: bytes, timestamp_ns: int) -> None:
    """Publish `payload` verbatim, bypassing the SDK's msgpack encode.

    ``NativeOutputs.write`` packs its argument; the in-process arm's
    ``write_raw`` does not. Going straight to the same FFI entry point the SDK
    itself calls is what keeps the two arms symmetric.
    """
    outputs = ctx.outputs
    buffer = (ctypes.c_uint8 * len(payload)).from_buffer_copy(payload)
    result = outputs._lib.slpn_output_write(
        outputs._ctx_ptr,
        port_name.encode("utf-8"),
        ctypes.cast(buffer, ctypes.c_void_p),
        len(payload),
        timestamp_ns,
    )
    if result != 0:
        raise RuntimeError(
            f"slpn_output_write refused a {len(payload)}-byte payload on "
            f"'{port_name}' with code {result}"
        )
