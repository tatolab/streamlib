# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`VirtualCameraSink` from Python: marker class to a camera other programs see.

The marker tests are pure Python and run in CI. The camera tests start a graph,
so they carry `requires_gpu` like every other graph test here — and they need
the one-time permission the sink itself never takes: the v4l2loopback module
loaded with its control node writable, which `streamlib enable-virtual-camera`
installs. Without it they skip naming the verb, since the refusal path is what
the engine's own unit tests already prove and a skipped test says why.

The reader side is a few V4L2 ioctls over `fcntl`, not a tool: what an
application sees is a `/dev/video*` node whose `card` is the configured name,
answering `VIDIOC_QUERYCAP` with capture, and handing back YUYV frames at the
frame's extent carrying the frame's own stamp. That is the whole contract, so
the test speaks it directly.
"""

import fcntl
import mmap
import os
import struct
import time
from pathlib import Path

import pytest

import streamlib

VIRTUAL_CAMERA_SINK_APP = Path(__file__).parent / "virtual_camera_sink_app.py"
CONTROL_NODE = Path("/dev/v4l2loopback")

# <linux/videodev2.h>: the ioctl numbers and struct layouts the reader speaks.
VIDIOC_QUERYCAP = 0x80685600
VIDIOC_S_FMT = 0xC0D05605
VIDIOC_REQBUFS = 0xC0145608
VIDIOC_QUERYBUF = 0xC0585609
VIDIOC_QBUF = 0xC058560F
VIDIOC_DQBUF = 0xC0585611
VIDIOC_STREAMON = 0x40045612
VIDIOC_STREAMOFF = 0x40045613
V4L2_BUF_TYPE_VIDEO_CAPTURE = 1
V4L2_MEMORY_MMAP = 1
V4L2_CAP_VIDEO_CAPTURE = 0x00000001
V4L2_CAP_VIDEO_OUTPUT = 0x00000002
V4L2_BUF_FLAG_TIMESTAMP_COPY = 0x00004000
YUYV = struct.unpack("<I", b"YUYV")[0]
V4L2_COLORSPACE_SRGB = 8
V4L2_XFER_FUNC_SRGB = 2
V4L2_YCBCR_ENC_601 = 1
V4L2_QUANTIZATION_FULL_RANGE = 1

# `struct v4l2_capability`: driver[16] card[32] bus_info[32] version caps device_caps reserved[3].
CAPABILITY_FORMAT = "16s32s32sIII3I"
# `struct v4l2_buffer` on 64-bit: index type bytesused flags field | timeval (two
# 8-byte words) | timecode (type flags frames seconds minutes hours userbits[4])
# sequence memory | m (an 8-byte union) length reserved2 request_fd — 88 bytes.
BUFFER_FORMAT = "IIIII" + "qq" + "II8s" + "II" + "Q" + "III"
# C pads the struct's tail to its 8-byte alignment; Python's native mode does not.
BUFFER_STRUCT_SIZE = 88
BUFFER_MEMORY_OFFSET = struct.calcsize("IIIIIqqII8sI")
BUFFER_M_OFFSET = struct.calcsize("IIIIIqqII8sII")
BUFFER_LENGTH_OFFSET = BUFFER_M_OFFSET + 8
assert struct.calcsize(BUFFER_FORMAT) == 84
assert (BUFFER_MEMORY_OFFSET, BUFFER_M_OFFSET, BUFFER_LENGTH_OFFSET) == (60, 64, 72)

# A camera that exists must appear within the sink's setup; one that was
# removed must be gone once the process has exited.
CAMERA_APPEARS_TIMEOUT_SECONDS = 30.0
CAMERA_POLL_INTERVAL_SECONDS = 0.25
FRAMES_TO_READ = 5
READ_TIMEOUT_SECONDS = 10.0


def control_node_is_writable() -> bool:
    return CONTROL_NODE.exists() and os.access(CONTROL_NODE, os.W_OK)


# The module's control ioctls, as `v4l2loopback.h` 0.15 declares them: query a
# device's config by number, remove a device by number (EBUSY while held).
V4L2LOOPBACK_CTL_QUERY = 0xC0487E03
V4L2LOOPBACK_CTL_REMOVE = 0x40047E02
V4L2LOOPBACK_CONFIG_SIZE = 72
V4L2LOOPBACK_CONFIG_LABEL_OFFSET = 8

# Every label a test here can create, so teardown recognises its own devices.
THIS_SUITES_LABEL_PREFIXES = ("StreamLib test ", "StreamLib frames ", "Refused cam")


def remove_loopback_devices_labelled_by_this_suite() -> None:
    """A test that kills its app skips the sink's teardown, so the device it
    created would outlive the run; this puts the rig back the way it was."""
    if not control_node_is_writable():
        return
    control = os.open(CONTROL_NODE, os.O_RDWR)
    try:
        for node in Path("/dev").glob("video*"):
            try:
                number = int(node.name.removeprefix("video"))
            except ValueError:
                continue
            config = bytearray(V4L2LOOPBACK_CONFIG_SIZE)
            struct.pack_into("i", config, 0, number)
            try:
                fcntl.ioctl(control, V4L2LOOPBACK_CTL_QUERY, config)
            except OSError:
                continue
            label = bytes(config[V4L2LOOPBACK_CONFIG_LABEL_OFFSET:V4L2LOOPBACK_CONFIG_LABEL_OFFSET + 32])
            label = label.split(b"\0", 1)[0].decode(errors="replace")
            if label.startswith(THIS_SUITES_LABEL_PREFIXES):
                # The app was just killed; its descriptor may close a beat later.
                deadline = time.monotonic() + 2.0
                while True:
                    try:
                        fcntl.ioctl(control, V4L2LOOPBACK_CTL_REMOVE, number)
                        break
                    except OSError as refusal:
                        if refusal.errno != 16 or time.monotonic() >= deadline:  # EBUSY
                            break
                        time.sleep(0.05)
    finally:
        os.close(control)


@pytest.fixture(autouse=True)
def leave_no_camera_behind():
    yield
    remove_loopback_devices_labelled_by_this_suite()


needs_the_loopback_permission = pytest.mark.skipif(
    not control_node_is_writable(),
    reason=(
        f"{CONTROL_NODE} is {'not writable by this user' if CONTROL_NODE.exists() else 'absent'}; "
        "run `streamlib enable-virtual-camera` once on this machine"
    ),
)


def query_capability(video_node: Path):
    """`(driver, card, capabilities)` of a V4L2 node, or `None` if it will not answer."""
    try:
        fd = os.open(video_node, os.O_RDWR | os.O_NONBLOCK)
    except OSError:
        return None
    try:
        raw = bytearray(struct.calcsize(CAPABILITY_FORMAT))
        fcntl.ioctl(fd, VIDIOC_QUERYCAP, raw)
    except OSError:
        return None
    finally:
        os.close(fd)
    driver, card, _bus, _version, capabilities, device_caps, *_ = struct.unpack(
        CAPABILITY_FORMAT, raw
    )
    return (
        driver.split(b"\0", 1)[0].decode(),
        card.split(b"\0", 1)[0].decode(),
        device_caps or capabilities,
    )


def video_nodes_named(camera_name: str) -> list[Path]:
    return [
        node
        for node in sorted(Path("/dev").glob("video*"))
        if (answer := query_capability(node)) is not None and answer[1] == camera_name
    ]


def await_camera(camera_name: str, present: bool, app) -> list[Path]:
    deadline = time.monotonic() + CAMERA_APPEARS_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        nodes = video_nodes_named(camera_name)
        if bool(nodes) == present:
            return nodes
        time.sleep(CAMERA_POLL_INTERVAL_SECONDS)
    raise AssertionError(
        f"camera {camera_name!r} {'never appeared' if present else 'is still present'}; "
        f"app output:\n{app.output}"
    )


def await_capture_capability(video_node: Path, app):
    """Other openers see CAPTURE once the writer is streaming; until the sink
    has queued its first frame the module still answers OUTPUT."""
    deadline = time.monotonic() + CAMERA_APPEARS_TIMEOUT_SECONDS
    answer = None
    while time.monotonic() < deadline:
        answer = query_capability(video_node)
        if answer is not None and answer[2] & V4L2_CAP_VIDEO_CAPTURE:
            return answer
        time.sleep(CAMERA_POLL_INTERVAL_SECONDS)
    raise AssertionError(
        f"{video_node} never announced capture (last answer {answer}); app output:\n{app.output}"
    )


def read_yuyv_frames(video_node: Path, count: int):
    """Capture `count` frames the way any V4L2 application would: negotiate
    the format the writer set, map the device's buffers, stream, dequeue.

    Returns `(width, height, sizeimage, (colorspace, ycbcr_enc, quantization,
    xfer_func), [(timestamp_ns, flags, bytesused, first_8_bytes)])`.
    """
    fd = os.open(video_node, os.O_RDWR | os.O_NONBLOCK)
    try:
        # `struct v4l2_format` is 208 bytes; the pix union starts at offset 8:
        # width height pixelformat field bytesperline sizeimage colorspace priv flags | ycbcr_enc quantization xfer_func.
        fmt = bytearray(208)
        struct.pack_into("I", fmt, 0, V4L2_BUF_TYPE_VIDEO_CAPTURE)
        struct.pack_into("III", fmt, 8, 0, 0, YUYV)
        fcntl.ioctl(fd, VIDIOC_S_FMT, fmt)
        width, height, pixelformat, _field, _bytesperline, sizeimage = struct.unpack_from(
            "IIIIII", fmt, 8
        )
        colorspace, _priv, _flags, ycbcr_enc, quantization, xfer_func = struct.unpack_from(
            "IIIIII", fmt, 32
        )
        signalled_color = (colorspace, ycbcr_enc, quantization, xfer_func)
        assert pixelformat == YUYV, f"the device set {pixelformat:#x}, not YUYV"

        request = bytearray(struct.pack("IIII4s", 4, V4L2_BUF_TYPE_VIDEO_CAPTURE, V4L2_MEMORY_MMAP, 0, b""))
        fcntl.ioctl(fd, VIDIOC_REQBUFS, request)
        (granted,) = struct.unpack_from("I", request, 0)
        assert granted > 0, "the device granted no capture buffers"

        mappings = []
        for index in range(granted):
            described = bytearray(BUFFER_STRUCT_SIZE)
            struct.pack_into("II", described, 0, index, V4L2_BUF_TYPE_VIDEO_CAPTURE)
            struct.pack_into("I", described, BUFFER_MEMORY_OFFSET, V4L2_MEMORY_MMAP)
            fcntl.ioctl(fd, VIDIOC_QUERYBUF, described)
            (offset,) = struct.unpack_from("I", described, BUFFER_M_OFFSET)
            (length,) = struct.unpack_from("I", described, BUFFER_LENGTH_OFFSET)
            mappings.append(mmap.mmap(fd, length, mmap.MAP_SHARED, mmap.PROT_READ, offset=offset))
            fcntl.ioctl(fd, VIDIOC_QBUF, described)
        fcntl.ioctl(fd, VIDIOC_STREAMON, struct.pack("i", V4L2_BUF_TYPE_VIDEO_CAPTURE))

        frames = []
        deadline = time.monotonic() + READ_TIMEOUT_SECONDS
        try:
            while len(frames) < count and time.monotonic() < deadline:
                dequeued = bytearray(BUFFER_STRUCT_SIZE)
                struct.pack_into("I", dequeued, 4, V4L2_BUF_TYPE_VIDEO_CAPTURE)
                struct.pack_into("I", dequeued, BUFFER_MEMORY_OFFSET, V4L2_MEMORY_MMAP)
                try:
                    fcntl.ioctl(fd, VIDIOC_DQBUF, dequeued)
                except BlockingIOError:
                    time.sleep(0.005)
                    continue
                index, _type, bytesused, flags, _field, tv_sec, tv_usec = struct.unpack_from(
                    "IIIIIqq", dequeued, 0
                )
                frames.append((tv_sec * 1_000_000_000 + tv_usec * 1_000, flags, bytesused, bytes(mappings[index][:8])))
                fcntl.ioctl(fd, VIDIOC_QBUF, dequeued)
        finally:
            fcntl.ioctl(fd, VIDIOC_STREAMOFF, struct.pack("i", V4L2_BUF_TYPE_VIDEO_CAPTURE))
            for mapping in mappings:
                mapping.close()
        return width, height, sizeimage, signalled_color, frames
    finally:
        os.close(fd)


# ---- marker semantics (no GPU) ---------------------------------------------


def test_the_marker_class_cannot_be_instantiated():
    with pytest.raises(TypeError):
        streamlib.VirtualCameraSink()


def test_display_name_defaults_to_the_type_name():
    runtime = streamlib.Runtime()
    try:
        sink = runtime.add(streamlib.VirtualCameraSink)
        assert sink.display_name == "VirtualCameraSink"
    finally:
        runtime.shutdown()


# ---- without the permission: a refusal by name, and the runtime keeps running


@pytest.mark.requires_gpu
@pytest.mark.skipif(
    control_node_is_writable(),
    reason=f"{CONTROL_NODE} is writable here, so the loopback door opens rather than refusing",
)
def test_without_the_permission_the_sink_refuses_naming_the_verb_and_the_runtime_keeps_running(
    start_app_under_test,
):
    """The complement of the camera tests: on a machine that has not run the
    verb, a sink forced onto the loopback door never reaches Running, the
    readiness wait raises with the sink's own text naming the verb, and the
    engine — still hosting the source — shuts down cleanly rather than dying."""
    app = start_app_under_test(VIRTUAL_CAMERA_SINK_APP, "--name", "Refused cam")

    app.await_output_containing("MARKER:NOT_EVERY_PROCESSOR_RUNNING", "the readiness refusal")
    refusal = next(
        line for line in app.output.splitlines() if "MARKER:NOT_EVERY_PROCESSOR_RUNNING" in line
    )
    assert "VirtualCameraSink" in refusal
    assert "no permission to create a v4l2loopback camera" in refusal, refusal
    assert "/dev/v4l2loopback is absent" in refusal or "not writable by this user" in refusal, refusal
    assert "streamlib enable-virtual-camera" in refusal, "the refusal names the one-time verb"

    app.await_clean_exit()
    assert "MARKER:EVERY_PROCESSOR_RUNNING" not in app.output


# ---- a camera other applications see (GPU + the loopback permission) -------


@pytest.mark.requires_gpu
@needs_the_loopback_permission
def test_a_camera_appears_while_the_graph_runs_and_is_gone_after_shutdown(
    start_app_under_test,
):
    camera_name = f"StreamLib test {os.getpid()}"
    second_name = f"{camera_name} B"
    assert not video_nodes_named(camera_name), "a stale camera carries this run's name"

    app = start_app_under_test(
        VIRTUAL_CAMERA_SINK_APP, "--name", camera_name, "--second-name", second_name
    )
    app.await_marker("EVERY_PROCESSOR_RUNNING")
    first = await_camera(camera_name, present=True, app=app)
    second = await_camera(second_name, present=True, app=app)
    assert len(first) == 1 and len(second) == 1, "two sinks are exactly two cameras"
    assert first != second

    driver, card, capabilities = await_capture_capability(first[0], app)
    assert driver == "v4l2 loopback"
    assert card == camera_name
    assert capabilities & V4L2_CAP_VIDEO_CAPTURE, "readers see a capture device"
    assert not capabilities & V4L2_CAP_VIDEO_OUTPUT, (
        "capture-only to readers — the mode Chromium's enumerator lists"
    )

    app.interrupt()
    app.await_clean_exit()
    assert not video_nodes_named(camera_name), f"the camera outlived its processor:\n{app.output}"
    assert not video_nodes_named(second_name)


@pytest.mark.requires_gpu
@needs_the_loopback_permission
def test_frames_reach_the_loopback_device_and_read_back_as_yuyv(start_app_under_test):
    camera_name = f"StreamLib frames {os.getpid()}"
    width, height = 640, 360
    app = start_app_under_test(
        VIRTUAL_CAMERA_SINK_APP,
        "--name", camera_name, "--width", str(width), "--height", str(height),
    )
    app.await_marker("EVERY_PROCESSOR_RUNNING")
    (node,) = await_camera(camera_name, present=True, app=app)
    await_capture_capability(node, app)

    read_started_ns = time.monotonic_ns()
    got_width, got_height, sizeimage, signalled_color, frames = read_yuyv_frames(
        node, FRAMES_TO_READ
    )
    read_finished_ns = time.monotonic_ns()

    # The test pattern publishes sRGB primaries and transfer at full range; the
    # kernel encodes with that resolved description and the device must say
    # so on every axis — a reader that derives limited range from the
    # colorspace would stretch the picture.
    assert signalled_color == (
        V4L2_COLORSPACE_SRGB,
        V4L2_YCBCR_ENC_601,
        V4L2_QUANTIZATION_FULL_RANGE,
        V4L2_XFER_FUNC_SRGB,
    ), f"the device signals {signalled_color}"

    assert (got_width, got_height) == (width, height), "the device format is the frame's extent"
    assert sizeimage == width * height * 2, "YUYV: two bytes a pixel"
    assert len(frames) == FRAMES_TO_READ, f"read {len(frames)} frames; app output:\n{app.output}"
    stamps = [stamp for stamp, _flags, _used, _head in frames]
    assert stamps == sorted(stamps) and len(set(stamps)) == len(stamps), (
        f"frame stamps advance: {stamps}"
    )
    for stamp, flags, bytesused, head in frames:
        assert flags & V4L2_BUF_FLAG_TIMESTAMP_COPY, "the writer's stamp is passed through"
        assert bytesused == sizeimage, "every frame is a whole picture"
        # Monotonic and current: the stamp names an instant inside this read,
        # give or take the source's publishing lead.
        assert read_started_ns - 5_000_000_000 < stamp < read_finished_ns + 5_000_000_000, (
            f"stamp {stamp} is not in the monotonic epoch this process reads"
        )
        # The pattern's left-most bar is white: at full range that is Y = 255
        # with neutral chroma, so the first macropixel is a known value.
        y0, u, y1, v = head[0], head[1], head[2], head[3]
        assert y0 >= 250 and y1 >= 250, f"white bar luma {y0},{y1}"
        assert abs(u - 128) <= 3 and abs(v - 128) <= 3, f"white bar chroma {u},{v}"

    app.interrupt()
    app.await_clean_exit()
