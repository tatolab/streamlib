# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Probes for a window a Python processor owns, from where one really runs.

The window is requested in `setup()` where the capability is Full and named
frames from `process()`; it lives in the app process on the engine's own
present loop, one hop away, so nothing here waits on a vsync.

What is worth breaking a build over: that all three shapes a caller can name a
published surface with reach the window — a cast object, a handle a kernel
wrote, and a bare id — that a close is never an exception a user's gesture
raised, and that a process which can get no window at all says so at `setup()`
rather than handing back a window that shows nothing.

Every probe runs in its own helper process and reports one
`MARKER:PROBE_RESULT` JSON line.
"""

import json
import os
import struct
import traceback

from streamlib import (
    GpuSurfaceHandle,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    VideoFrame,
    input,
    log,
    processor,
)
from streamlib._engine import ComputeKernel, ProcessorOwnedWindow

RESULT_MARKER = "MARKER:PROBE_RESULT "
# A second line, and only for the gesture: the result line is reported once
# from a probe that goes on presenting, so the close needs a marker of its
# own or it would have nowhere to be seen.
CLOSE_MARKER = "MARKER:THE_USER_CLOSED_THE_WINDOW "

WINDOW_TITLE = "streamlib processor-owned window"
REQUESTED_WINDOW_WIDTH = 640
REQUESTED_WINDOW_HEIGHT = 480

KERNEL_OUTPUT_WIDTH = 256
KERNEL_OUTPUT_HEIGHT = 256

# A kernel output the window can actually present: a render-target-capable
# texture the processor owns, written by its own dispatch. The gradient moves
# with the frame counter so the debug window is visibly live on the rig rather
# than a still that a wedged present loop would be indistinguishable from.
MOVING_GRADIENT_GLSL = """\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0, rgba8) uniform writeonly image2D output_image;
layout(push_constant) uniform PushConstants { float phase; } pc;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    ivec2 extent = imageSize(output_image);
    if (at.x >= extent.x || at.y >= extent.y) { return; }
    vec2 uv = (vec2(at) + 0.5) / vec2(extent);
    imageStore(output_image, at, vec4(uv.x, uv.y, fract(pc.phase), 1.0));
}
"""

KERNEL_OUTPUT_BINDING = "output_image"
KERNEL_OUTPUT_USAGE = [
    "texture_binding",
    "storage_binding",
    "copy_src",
    "copy_dst",
]


def _report(probe_body) -> None:
    """One result line per probe, success or failure — the failure carries the
    traceback so the test fails on the cause rather than a missing marker."""
    try:
        observation = probe_body()
    except BaseException:  # noqa: BLE001 — re-raised by the asserting test
        observation = {"failure": traceback.format_exc()}
    log.info(RESULT_MARKER + json.dumps({"pid": os.getpid(), **observation}))


def _refusal_of(window_call) -> str:
    """The message a call raises, or a failure if it did not raise."""
    try:
        window_call()
    except Exception as refusal:  # noqa: BLE001 — the refusal is the subject
        return str(refusal)
    raise AssertionError("the call was accepted; it should have been refused")


class _WindowOwningProbeBase:
    """Owns a window from `setup`, drives it per frame from `process`.

    Reports once and keeps presenting afterwards: a debug window whose owner
    stopped naming frames is exactly the thing a stalled present loop looks
    like, and the live harness photographs this same class.
    """

    # Declared, not merely assigned: `setup` assigns these inside a nested
    # closure, which a type checker does not walk for attribute inference.
    debug_window: ProcessorOwnedWindow
    gradient_kernel: ComputeKernel
    kernel_output: GpuSurfaceHandle

    @input(delivery_profile="latest")
    def video_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self.frames_seen = 0
        self.reported = False

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        gpu = ctx.gpu_full_access
        # Raises here when the process can get no window — which is the
        # contract, and why the optional-window probe is the one that catches.
        self.debug_window = gpu.create_window(
            title=WINDOW_TITLE,
            width=REQUESTED_WINDOW_WIDTH,
            height=REQUESTED_WINDOW_HEIGHT,
        )
        self.gradient_kernel = gpu.create_compute_kernel(
            source=MOVING_GRADIENT_GLSL,
            push_constant_size=4,
            bindings={KERNEL_OUTPUT_BINDING: "storage_image"},
        )
        self.kernel_output = gpu.acquire_texture(
            KERNEL_OUTPUT_WIDTH,
            KERNEL_OUTPUT_HEIGHT,
            "rgba8_unorm",
            KERNEL_OUTPUT_USAGE,
        )

    def _dispatch_the_gradient(self) -> None:
        """One dispatch into the processor's own output texture."""
        self.gradient_kernel.dispatch(
            bindings={KERNEL_OUTPUT_BINDING: self.kernel_output},
            group_count=(KERNEL_OUTPUT_WIDTH // 8, KERNEL_OUTPUT_HEIGHT // 8, 1),
            push_constants=struct.pack("<f", self.frames_seen / 60.0),
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        frame = ctx.inputs.read("video_from_upstream", into=VideoFrame)
        if frame is None:
            return
        self.frames_seen += 1
        try:
            self._drive(frame)
        except BaseException:  # noqa: BLE001 — surfaced through the marker line
            if not self.reported:
                self.reported = True
                log.info(
                    RESULT_MARKER
                    + json.dumps({"pid": os.getpid(), "failure": traceback.format_exc()})
                )

    def _drive(self, frame: VideoFrame) -> None:
        raise NotImplementedError


@processor
class EveryArgumentShapeReachesTheWindowProbe(_WindowOwningProbeBase):
    """All three shapes that name a published surface, one per frame.

    Then it settles on the delivered frame and keeps naming it, watching for
    the user's own close — which is what makes this class the live harness as
    well as the assertion: the window shows the source until someone shuts it.
    """

    def __init__(self) -> None:
        super().__init__()
        self.shapes_accepted: "list[str]" = []
        self.drained_width = 0
        self.drained_height = 0
        self.reported_the_users_close = False
        self.the_user_asked_to_close_the_window = False
        self.close_gestures_reported = 0

    def _drive(self, frame: VideoFrame) -> None:
        self._dispatch_the_gradient()
        if self.frames_seen == 1:
            self.debug_window.show(self.kernel_output)
            self.shapes_accepted.append("kernel_output_handle")
        elif self.frames_seen == 2:
            self.debug_window.show(frame)
            self.shapes_accepted.append("cast_object")
        elif self.frames_seen == 3:
            # The kernel's own output rather than the delivered frame's id: a
            # bare id carries no extent, and the host reads that as "a
            # buffer-backed surface is not acceptable to me". The escape hatch
            # is for a surface the caller knows is texture-backed, which a
            # kernel output always is.
            self.debug_window.show(self.kernel_output.surface_id)
            self.shapes_accepted.append("bare_surface_id")
        else:
            # The debug window a user would actually leave up.
            self.debug_window.show(frame)

        if self.frames_seen < 3:
            return
        events = self.debug_window.drain_events()
        if not self.reported:
            self.drained_width = events.current_width_in_physical_pixels
            self.drained_height = events.current_height_in_physical_pixels
            self.reported = True
            _report(
                lambda: {
                    "shapes_accepted": self.shapes_accepted,
                    "window_title": self.debug_window.title,
                    "is_closed": self.debug_window.is_closed,
                    "drained_width": self.drained_width,
                    "drained_height": self.drained_height,
                    "close_requested_by_user": events.close_requested_by_user,
                    "window_is_closed": events.window_is_closed,
                }
            )
            return
        self._react_to_the_users_close(events, frame)

    def _react_to_the_users_close(self, events, frame: VideoFrame) -> None:
        """The owner's close policy: react, never prevent.

        The gesture arrives first and the closed flag follows it — the engine
        stops the window's present loop when the request lands, and the window
        is closed once that thread has left — so an owner watching for both
        sees them on different drains. All it does with either is notice.
        """
        if events.close_requested_by_user:
            self.close_gestures_reported += 1
            self.the_user_asked_to_close_the_window = True
        if (
            self.reported_the_users_close
            or not self.the_user_asked_to_close_the_window
            or not events.window_is_closed
        ):
            return
        self.reported_the_users_close = True
        log.info(
            CLOSE_MARKER
            + json.dumps(
                {
                    "pid": os.getpid(),
                    "close_gestures_reported": self.close_gestures_reported,
                    "window_is_closed": events.window_is_closed,
                    "is_closed": self.debug_window.is_closed,
                    "frames_seen_when_the_user_closed_it": self.frames_seen,
                }
            )
        )
        # The no-op, in the per-frame path where it matters: naming a frame to
        # a window the user shut must never take the pipeline down.
        self.debug_window.show(frame)
        self.debug_window.show(self.kernel_output)


@processor
class AnOwnerClosingItsOwnWindowProbe(_WindowOwningProbeBase):
    """A close leaves the pipeline running and every later `show()` a no-op.

    The owner's own close rather than the user's gesture: both leave the
    window in the same state, and only one of them a test can perform without
    a hand on a mouse. The gesture is the live arm.
    """

    def _drive(self, frame: VideoFrame) -> None:
        if self.reported:
            # The pipeline is still running, which is the other half of the
            # claim: a closed window took nothing down with it.
            return
        self.debug_window.show(frame)
        closed_before = self.debug_window.is_closed
        self.debug_window.close()
        closed_after_close = self.debug_window.is_closed
        # The no-op, and the reason it is one: a user's gesture must never
        # become an exception in a processor's per-frame path.
        self.debug_window.show(frame)
        self.debug_window.show(self.kernel_output)
        self.debug_window.show(frame.surface_id)
        events = self.debug_window.drain_events()
        # Never an error for a window already closed.
        self.debug_window.close()
        self.reported = True
        _report(
            lambda: {
                "closed_before_close": closed_before,
                "closed_after_close": closed_after_close,
                "window_is_closed_after_drain": events.window_is_closed,
                "frames_seen": self.frames_seen,
            }
        )


@processor
class AProcessThatCanGetNoWindowRefusesAtSetupProbe:
    """The optional-window pattern, written the way an author writes it.

    No `_WindowOwningProbeBase`: the whole subject is that `setup()` raises
    before there is a window to own, and that an author who considers the
    window optional carries on without one.
    """

    @input(delivery_profile="latest")
    def video_from_upstream(self) -> None: ...

    def __init__(self) -> None:
        self.debug_window: "ProcessorOwnedWindow | None" = None
        self.refusal = ""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        try:
            self.debug_window = ctx.gpu_full_access.create_window(
                title=WINDOW_TITLE,
                width=REQUESTED_WINDOW_WIDTH,
                height=REQUESTED_WINDOW_HEIGHT,
            )
        except RuntimeError as refusal:
            # The whole optional-window pattern: no window, no exception out
            # of `setup`, and a processor that goes on doing its work.
            self.debug_window = None
            self.refusal = str(refusal)
        _report(
            lambda: {
                "window_was_granted": self.debug_window is not None,
                "refusal": self.refusal,
            }
        )

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        pass


@processor
class ShowingSomethingThatNamesNoSurfaceIsRefusedProbe(_WindowOwningProbeBase):
    """A window refuses what names no published surface, in the caller's own
    stack rather than a round trip later."""

    def _drive(self, frame: VideoFrame) -> None:
        if self.reported:
            self.debug_window.show(frame)
            return
        self.reported = True
        _report(
            lambda: {
                # The stub forbids both, which is the first line of defence
                # and not the one under test: an author reaches this refusal
                # from untyped code, or from a variable a checker widened.
                "refusal_for_an_object_naming_nothing": _refusal_of(
                    lambda: self.debug_window.show(object())  # pyright: ignore[reportArgumentType]
                ),
                "refusal_for_an_integer": _refusal_of(
                    lambda: self.debug_window.show(7)  # pyright: ignore[reportArgumentType]
                ),
            }
        )


@processor
class AFrameDescribingItsColourReachesTheWindowProbe(_WindowOwningProbeBase):
    """A frame that names its colour and carries an HDR sidecar.

    The wire's own golden vector proves the host parses this document; the
    builder's unit tests prove the numbers. What only a live hop can prove is
    that this side emits the keys the host reads — a typo in one of the eight
    ST.2086 fields raises nowhere until a real HDR frame is shown, and then it
    raises in a user's `process()`.

    Its own cast type rather than the delivered `VideoFrame`, because the test
    pattern and the camera both describe SDR: the description has to be one the
    probe chooses.
    """

    def _drive(self, frame: VideoFrame) -> None:
        described = _AnHdrFrameNaming(self.kernel_output.surface_id)
        self.debug_window.show(described)  # pyright: ignore[reportArgumentType]
        if self.reported:
            return
        self.reported = True
        _report(
            lambda: {
                "the_described_frame_was_accepted": True,
                "is_closed": self.debug_window.is_closed,
            }
        )


class _AnHdrFrameNaming:
    """A cast type of the probe's own, describing a PQ / BT.2020 frame.

    Written out rather than composed from `ClaimedSurfacePixelAccess` because
    the claim needs a typed read to offer it, and this frame names a texture
    the processor already owns — nothing to protect from a producer.
    """

    class color_info:
        primaries = "bt2020"
        # H.273's own name for PQ. That the wire carries `pq` instead is the
        # collapse this probe is here to see survive the hop.
        transfer = "smpte2084"

    class mastering_display:
        display_primaries_r_x = 35_400
        display_primaries_r_y = 14_600
        display_primaries_g_x = 8_500
        display_primaries_g_y = 39_850
        display_primaries_b_x = 6_550
        display_primaries_b_y = 2_300
        white_point_x = 15_635
        white_point_y = 16_450
        max_luminance = 10_000_000
        min_luminance = 50

    class content_light:
        max_cll = 1_000
        max_fall = 400

    def __init__(self, surface_id: str) -> None:
        self.surface_id_the_claim_was_taken_on = surface_id
        self.width = KERNEL_OUTPUT_WIDTH
        self.height = KERNEL_OUTPUT_HEIGHT
