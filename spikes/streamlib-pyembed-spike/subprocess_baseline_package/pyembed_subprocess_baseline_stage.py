# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The #1702 baseline arm: the spike's stage callback, hosted in a subprocess.

The same shape of work the in-process arm does per frame — read the payload,
hand a numpy view of the frame's pixels to the same callable from
``spike_stage_callbacks``, patch the stage duration into the measurement
preamble, write the payload on — hosted the way today's model hosts it: a
separate process, a per-package venv, and the payload crossing iceoryx2 through
the python-native cdylib rather than staying in one address space.

Two departures from the SDK's public surface, both taken so the two arms measure
the same quantity:

* ``inputs._read_raw`` / ``slpn_output_write`` are used directly instead of
  ``inputs.read`` / ``outputs.write``. The public pair msgpack-encodes and
  decodes, which the in-process arm's ``read_raw``/``write_raw`` do not — the
  encode would land inside the measured span and show up as PyO3 winning.
* The pixels the callback views come from a process-local buffer allocated at
  setup, exactly as on the in-process arm.

The per-frame work is NOT byte-for-byte identical across the arms, and the
residual asymmetries pull in both directions. Against the in-process arm: it
builds a fresh numpy array per frame and runs a refcount escape check, neither
of which this stage does (it reuses one array from setup). Against this arm: it
makes three payload copies per frame (``bytearray``, ``bytes``, and
``from_buffer_copy``) and drains a lifecycle queue, none of which the in-process
arm has. Separately, a real subprocess would pay a cpu-readback escalate round
trip to reach a GPU surface's pixels, which this stage does not — that one
biases *toward* the baseline, which is the safe direction: a GO decided against
a conservatively-fast baseline is still a GO.
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

# Both arrive in the config from the Rust side rather than being re-typed here.
# The pattern modulus is load-bearing: both Python arms must hand the callback
# identical bytes, or the arm comparison stops comparing like with like. The
# fallbacks exist only so a hand-written config still runs.
DEFAULT_SYNTHETIC_PIXEL_PATTERN_MODULUS = 251
DEFAULT_SURFACE_REFERENCE_BODY_BYTES = 192


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

        self._surface_reference_body_bytes = int(
            config.get(
                "surface_reference_body_bytes", DEFAULT_SURFACE_REFERENCE_BODY_BYTES
            )
        )
        pattern_modulus = int(
            config.get(
                "synthetic_pixel_pattern_modulus",
                DEFAULT_SYNTHETIC_PIXEL_PATTERN_MODULUS,
            )
        )

        pixel_byte_count = (
            self._frame_width_pixels * self._frame_height_pixels * self._channel_count
        )
        # Same non-uniform fill the in-process arm's locally resolved surface
        # carries, so a callback that inspects content sees identical input.
        pattern = bytes((index % pattern_modulus) for index in range(pattern_modulus))
        repeats = pixel_byte_count // len(pattern) + 1
        self._locally_resolved_surface_pixels = numpy.frombuffer(
            (pattern * repeats)[:pixel_byte_count], dtype=numpy.uint8
        ).reshape(
            self._frame_height_pixels, self._frame_width_pixels, self._channel_count
        ).copy()

        # Resolved at setup, never per frame: the guard below runs on the hot
        # path and `.tobytes()` there would allocate a full picture every frame.
        self._expected_wire_body_byte_count = (
            pixel_byte_count
            if self._wire_payload_mode == "full-pixel-payload"
            else self._surface_reference_body_bytes
        )
        self._observed_frame_count = 0

    def process(self, ctx) -> None:
        import numpy

        frame_payload, frame_timestamp_ns = ctx.inputs._read_raw("frame_in")
        if frame_payload is None:
            return
        wire_body_byte_count = len(frame_payload) - MEASUREMENT_PREAMBLE_BYTES
        if wire_body_byte_count != self._expected_wire_body_byte_count:
            raise ValueError(
                f"stage is configured for {self._wire_payload_mode!r} and expects a "
                f"{self._expected_wire_body_byte_count}-byte wire body, but "
                f"{wire_body_byte_count} bytes arrived — the source and stage "
                "disagree about the wire payload mode"
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
