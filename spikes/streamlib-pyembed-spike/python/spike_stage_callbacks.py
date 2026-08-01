# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The per-frame stage callbacks the embedded interpreter invokes.

Kept out of runner.py deliberately: the Rust harness imports this module from
inside the embedded interpreter, and runner.py owns an argparse CLI and a
__main__ block that must not execute on import.

Every callback mutates its argument in place. The array aliases a Rust-owned
buffer for the duration of the call only, so rebinding the name discards the
work silently and retaining the array past the call is a use-after-free the Rust
side detects and reports but cannot prevent.
"""

import numpy

FRAME_CHANNEL_COUNT = 4

# A gain-and-bias pass, chosen to land in the protocol's 2-5ms band for
# 1920x1080x4 uint8 while staying a plausible real effect rather than a
# synthetic spin. Integer ops on an int16 intermediate avoid float conversion
# cost dominating the measurement.
REALISTIC_STAGE_GAIN_NUMERATOR = 5
REALISTIC_STAGE_GAIN_DENOMINATOR = 4
REALISTIC_STAGE_BRIGHTNESS_BIAS = 8

# Scratch buffers are allocated once per frame shape and reused. A per-frame
# 16 MB int16 temporary would turn this cell into an allocator benchmark: the
# in-process arm shares the engine's allocator and the subprocess arm does not,
# so the churn would land unevenly on the two arms' tails and confound exactly
# the comparison being made.
_intermediate_pixel_scratch_by_frame_shape = {}


def passthrough_stage(frame_pixel_array):
    """The zero-work stage: its whole cost is GIL acquisition plus building the
    numpy view over the Rust buffer."""
    return None


def realistic_stage(frame_pixel_array):
    """The ~2-5ms numpy stage, operating in place on the aliased Rust buffer."""
    frame_pixel_view = _reshape_frame_pixel_array_without_copying(frame_pixel_array)
    intermediate_pixel_scratch = _intermediate_pixel_scratch_for_shape(
        frame_pixel_view.shape
    )
    numpy.copyto(intermediate_pixel_scratch, frame_pixel_view, casting="unsafe")
    numpy.multiply(
        intermediate_pixel_scratch,
        REALISTIC_STAGE_GAIN_NUMERATOR,
        out=intermediate_pixel_scratch,
    )
    numpy.floor_divide(
        intermediate_pixel_scratch,
        REALISTIC_STAGE_GAIN_DENOMINATOR,
        out=intermediate_pixel_scratch,
    )
    numpy.add(
        intermediate_pixel_scratch,
        REALISTIC_STAGE_BRIGHTNESS_BIAS,
        out=intermediate_pixel_scratch,
    )
    numpy.clip(intermediate_pixel_scratch, 0, 255, out=intermediate_pixel_scratch)
    numpy.copyto(frame_pixel_view, intermediate_pixel_scratch, casting="unsafe")
    return None


def build_measurement_stage_callback_for_stage_name(stage_name):
    """Resolve a `--stage` value to the callable the harness invokes per frame."""
    if stage_name == "passthrough":
        return passthrough_stage
    if stage_name == "realistic":
        return realistic_stage
    raise ValueError(
        f"unsupported stage name {stage_name!r}; expected 'passthrough' or 'realistic'"
    )


def _intermediate_pixel_scratch_for_shape(frame_shape):
    scratch = _intermediate_pixel_scratch_by_frame_shape.get(frame_shape)
    if scratch is None:
        scratch = numpy.empty(frame_shape, dtype=numpy.int16)
        _intermediate_pixel_scratch_by_frame_shape[frame_shape] = scratch
    return scratch


def _reshape_frame_pixel_array_without_copying(frame_pixel_array):
    if frame_pixel_array.ndim == 3:
        return frame_pixel_array
    if frame_pixel_array.size % FRAME_CHANNEL_COUNT != 0:
        raise ValueError(
            f"frame carries {frame_pixel_array.size} elements, "
            f"not a multiple of {FRAME_CHANNEL_COUNT} channels"
        )
    # numpy.reshape falls back to a copy when the source is not C-contiguous,
    # and a copy would send every pixel write to a temporary the Rust side never
    # reads. Refuse rather than silently produce a no-op stage.
    if not frame_pixel_array.flags.c_contiguous:
        raise ValueError(
            "frame buffer is not C-contiguous; reshaping it would copy and the "
            "stage would no longer write through to the Rust buffer"
        )
    return frame_pixel_array.reshape(
        (1, frame_pixel_array.size // FRAME_CHANNEL_COUNT, FRAME_CHANNEL_COUNT)
    )
