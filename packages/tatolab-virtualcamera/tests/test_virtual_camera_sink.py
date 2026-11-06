# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""`VirtualCameraSink`'s declaration, config, door rule, naming, colour fields and kernel ABI, with no GPU and no module.

The camera itself is proven on a runtime with the module loaded, by the
integration suite's `tests/stream-on-runtime/test_virtual_camera_sink.py`.
"""

import ctypes
import errno
import importlib.metadata

import pytest
from tatolab.stream import ColorInfo, StreamBuilder, compile_stream_to_graph, stream
from tatolab.stream._node_config_schema import derive_config_class_json_schema
from tatolab.stream._node_declaration import (
    NODE_DECLARATION_EXECUTION_STAMP,
    NODE_DECLARATION_INPUT_PORTS_STAMP,
    NODE_DECLARATION_OUTPUT_PORTS_STAMP,
)
from tatolab.virtualcamera import (
    VirtualCameraSink,
    VirtualCameraSinkConfig,
    copy_rows_between,
    loopback_door_refusal_or_none,
    neither_door_opens_refusal,
    virtual_camera_door_taken,
)
from tatolab.virtualcamera import v4l2_output_device_abi as abi
from tatolab.virtualcamera.v4l2_color_fields import (
    V4l2PixFormatColorFields,
    v4l2_color_fields_of,
    yuyv_picture_color_of,
)
from tatolab.virtualcamera.v4l2_loopback_output_device import (
    yuyv_picture_byte_count_or_none,
)
from tatolab.virtualcamera.v4l2loopback_control_node import (
    ControlNodeAccess,
    create_or_reclaim_device_then,
    find_device_carrying_label,
    loopback_config_for_a_new_camera,
    removal_was_refused_because_a_reader_holds_the_device,
)
from tatolab.virtualcamera.virtual_camera_naming import (
    TheNodeAVirtualCameraIsNamedFor,
    camera_name_for,
    fnv1a_64_hash_of,
)


def test_the_only_port_is_one_newest_image_input_and_the_node_is_reactive():
    input_ports = getattr(VirtualCameraSink, NODE_DECLARATION_INPUT_PORTS_STAMP)

    assert [(port["name"], port["delivery_profile"]) for port in input_ports] == [
        ("video", "newest")
    ]
    assert input_ports[0]["family"] == {"family": "image"}
    assert getattr(VirtualCameraSink, NODE_DECLARATION_OUTPUT_PORTS_STAMP) == []
    assert (
        getattr(VirtualCameraSink, NODE_DECLARATION_EXECUTION_STAMP)["mode"]
        == "reactive"
    )


@stream
def a_virtual_camera(stream_builder: StreamBuilder) -> None:
    stream_builder.add(VirtualCameraSink, config={"name": "Desk cam"})


def test_a_stream_names_the_sink_by_the_type_the_module_publishes():
    graph = compile_stream_to_graph(a_virtual_camera)

    assert [
        (node["name"], node["type"], node["config"]) for node in graph["nodes"]
    ] == [
        (
            "virtualcamerasink",
            "tatolab.virtualcamera:VirtualCameraSink",
            {"name": "Desk cam"},
        )
    ]


def test_the_config_names_the_camera_and_the_door_and_nothing_else():
    config_schema = derive_config_class_json_schema(VirtualCameraSinkConfig)

    assert set(config_schema["properties"]) == {"name", "door"}
    assert config_schema["properties"]["door"]["default"] == "auto"
    assert VirtualCameraSinkConfig() == VirtualCameraSinkConfig(name=None, door="auto")
    with pytest.raises(ValueError, match="door"):
        VirtualCameraSinkConfig(door="both")  # type: ignore[arg-type]
    with pytest.raises(ValueError, match="name"):
        VirtualCameraSinkConfig(name=7)  # type: ignore[arg-type]
    with pytest.raises(TypeError, match="fps"):
        VirtualCameraSinkConfig(fps=60)  # type: ignore[call-arg]


def test_a_writable_control_node_opens_the_loopback_door():
    assert loopback_door_refusal_or_none("Desk cam", ControlNodeAccess.WRITABLE) is None


@pytest.mark.parametrize(
    "access", [ControlNodeAccess.ABSENT, ControlNodeAccess.NOT_WRITABLE]
)
def test_no_permission_refuses_naming_the_camera_the_node_and_the_verb(access):
    refusal = loopback_door_refusal_or_none("Desk cam", access)

    assert refusal is not None
    assert '"Desk cam"' in refusal
    assert f"/dev/v4l2loopback is {access.value}" in refusal
    assert "`tatolab enable-virtual-camera`" in refusal


@pytest.mark.parametrize("access", list(ControlNodeAccess))
@pytest.mark.parametrize("door", ["v4l2loopback", "pipewire"])
def test_a_named_door_is_taken_whatever_the_control_node_says(door, access):
    assert virtual_camera_door_taken(door, access) == door


def test_auto_takes_the_loopback_door_where_it_is_permitted():
    assert (
        virtual_camera_door_taken("auto", ControlNodeAccess.WRITABLE) == "v4l2loopback"
    )


@pytest.mark.parametrize(
    "access", [ControlNodeAccess.ABSENT, ControlNodeAccess.NOT_WRITABLE]
)
def test_auto_takes_the_pipewire_door_where_the_loopback_door_is_not_permitted(access):
    assert virtual_camera_door_taken("auto", access) == "pipewire"


def test_auto_with_neither_door_names_both_refusals_and_the_verb():
    loopback_refusal = loopback_door_refusal_or_none(
        "Desk cam", ControlNodeAccess.ABSENT
    )
    assert loopback_refusal is not None

    refusal = neither_door_opens_refusal(
        "Desk cam", loopback_refusal, "no PipeWire daemon answers"
    )

    assert '"Desk cam": neither camera door opens' in refusal
    assert "`tatolab enable-virtual-camera`" in refusal
    assert "no PipeWire daemon answers" in refusal


def the_node_named(node_name: str) -> TheNodeAVirtualCameraIsNamedFor:
    return TheNodeAVirtualCameraIsNamedFor(
        stream_project_directory="/home/someone/apps/desk",
        stream_id="main",
        node_name=node_name,
    )


def test_the_hash_is_the_engines_fnv1a_64():
    assert fnv1a_64_hash_of(b"") == 0xCBF29CE484222325
    assert fnv1a_64_hash_of(b"a") == 0xAF63DC4C8601EC8C
    assert fnv1a_64_hash_of(b"foobar") == 0x85944171F73967E8


def test_an_unnamed_camera_gets_a_stable_id_that_differs_between_instances():
    first = camera_name_for(None, the_node_named("virtualcamerasink"))

    assert first == "StreamLib Camera vj1j"
    assert first != camera_name_for(None, the_node_named("virtualcamerasink-2"))
    assert first != camera_name_for(
        None,
        TheNodeAVirtualCameraIsNamedFor(
            "/home/someone/apps/lab", "main", "virtualcamerasink"
        ),
    )
    assert first != camera_name_for(
        None,
        TheNodeAVirtualCameraIsNamedFor(
            "/home/someone/apps/desk", "preview", "virtualcamerasink"
        ),
    )
    assert camera_name_for("   ", the_node_named("virtualcamerasink")) == first


def test_a_configured_name_is_capped_to_the_label_on_a_character_boundary():
    assert camera_name_for("  Desk cam ", the_node_named("x")) == "Desk cam"
    long_name = camera_name_for("Desk camera " * 4, the_node_named("x"))
    assert len(long_name.encode()) <= 31
    assert loopback_config_for_a_new_camera(long_name).label() == long_name
    assert camera_name_for("é" * 20, the_node_named("x")) == "é" * 15


class FakeControlNode:
    def __init__(self, devices: "dict[int, str]") -> None:
        self.devices = dict(devices)
        self.added: "list[str]" = []
        self.removed: "list[int]" = []

    def access(self) -> ControlNodeAccess:
        return ControlNodeAccess.WRITABLE

    def candidate_device_numbers(self) -> "list[int]":
        return sorted(self.devices)

    def query_device(self, device_number: int) -> "abi.V4l2LoopbackConfig | None":
        label = self.devices.get(device_number)
        return None if label is None else loopback_config_for_a_new_camera(label)

    def add_device(self, config: abi.V4l2LoopbackConfig) -> int:
        self.added.append(config.label())
        return 42

    def remove_device(self, device_number: int) -> None:
        self.removed.append(device_number)

    def module_version(self) -> "str | None":
        return "0.15.0"


def test_a_device_carrying_this_sinks_name_is_reclaimed_rather_than_duplicated():
    control_node = FakeControlNode({10: "Other cam", 11: "Desk cam"})

    assert find_device_carrying_label(control_node, "Desk cam") == 11
    assert find_device_carrying_label(control_node, "Nobody's cam") is None
    assert create_or_reclaim_device_then(
        control_node, "Desk cam", lambda n, r: (n, r)
    ) == (11, True)
    assert control_node.added == []


def test_a_setup_that_fails_after_creating_its_device_removes_it_and_leaves_a_reclaimed_one():
    control_node = FakeControlNode({11: "Desk cam"})

    def fail(device_number: int, reclaimed: bool) -> None:
        raise RuntimeError(f"open of {device_number} failed, reclaimed={reclaimed}")

    with pytest.raises(RuntimeError, match="42 failed, reclaimed=False"):
        create_or_reclaim_device_then(control_node, "New cam", fail)
    assert (control_node.added, control_node.removed) == (["New cam"], [42])
    with pytest.raises(RuntimeError, match="11 failed, reclaimed=True"):
        create_or_reclaim_device_then(control_node, "Desk cam", fail)
    assert control_node.removed == [42]
    assert (
        create_or_reclaim_device_then(control_node, "Third cam", lambda n, r: n) == 42
    )
    assert control_node.removed == [42]


def test_a_busy_removal_is_the_reader_still_holding_the_camera():
    assert removal_was_refused_because_a_reader_holds_the_device(
        OSError(errno.EBUSY, "busy")
    )
    assert not removal_was_refused_because_a_reader_holds_the_device(
        OSError(errno.EINVAL, "x")
    )


def test_a_new_camera_asks_for_the_next_free_device_and_four_buffers():
    config = loopback_config_for_a_new_camera("Desk cam")

    assert (config.output_nr, config.max_buffers, config.announce_all_caps) == (
        -1,
        4,
        0,
    )
    assert config.label() == "Desk cam"


@pytest.mark.parametrize(
    "structure, size",
    [
        (abi.V4l2Capability, 104),
        (abi.V4l2PixFormat, 48),
        (abi.V4l2Format, 208),
        (abi.V4l2RequestBuffers, 20),
        (abi.V4l2Buffer, 88),
        (abi.V4l2LoopbackConfig, 72),
    ],
)
def test_every_struct_is_the_size_the_64_bit_kernel_abi_declares(structure, size):
    assert ctypes.sizeof(structure) == size


def test_the_buffer_fields_sit_at_the_kernels_offsets():
    offsets = {
        name: getattr(abi.V4l2Buffer, name).offset
        for name in ("timestamp", "sequence", "memory", "m", "length", "request")
    }

    assert offsets == {
        "timestamp": 24,
        "sequence": 56,
        "memory": 60,
        "m": 64,
        "length": 72,
        "request": 80,
    }
    assert abi.V4l2LoopbackConfig.card_label.offset == 8
    assert abi.V4l2LoopbackConfig.max_buffers.offset == 56


def test_every_request_number_is_the_one_the_headers_define():
    assert {
        "QUERYCAP": abi.VIDIOC_QUERYCAP,
        "S_FMT": abi.VIDIOC_S_FMT,
        "REQBUFS": abi.VIDIOC_REQBUFS,
        "QUERYBUF": abi.VIDIOC_QUERYBUF,
        "QBUF": abi.VIDIOC_QBUF,
        "DQBUF": abi.VIDIOC_DQBUF,
        "STREAMON": abi.VIDIOC_STREAMON,
        "STREAMOFF": abi.VIDIOC_STREAMOFF,
        "CTL_VERSION": abi.V4L2LOOPBACK_CTL_VERSION,
        "CTL_ADD": abi.V4L2LOOPBACK_CTL_ADD,
        "CTL_REMOVE": abi.V4L2LOOPBACK_CTL_REMOVE,
        "CTL_QUERY": abi.V4L2LOOPBACK_CTL_QUERY,
    } == {
        "QUERYCAP": 0x80685600,
        "S_FMT": 0xC0D05605,
        "REQBUFS": 0xC0145608,
        "QUERYBUF": 0xC0585609,
        "QBUF": 0xC058560F,
        "DQBUF": 0xC0585611,
        "STREAMON": 0x40045612,
        "STREAMOFF": 0x40045613,
        "CTL_VERSION": 0x80047E00,
        "CTL_ADD": 0x40487E01,
        "CTL_REMOVE": 0x40047E02,
        "CTL_QUERY": 0xC0487E03,
    }
    assert abi.V4L2_PIX_FMT_YUYV == 0x56595559


@pytest.mark.parametrize(
    "width, height, byte_count",
    [
        (1280, 720, 1_843_200),
        (1281, 720, None),
        (0, 720, None),
        (1280, 0, None),
        (65_536, 32_768, None),
    ],
)
def test_a_picture_a_yuyv_camera_cannot_carry_has_no_size(width, height, byte_count):
    assert yuyv_picture_byte_count_or_none(width, height) == byte_count


def test_an_absent_colour_axis_takes_the_engines_ycbcr_default():
    assert yuyv_picture_color_of(None) == ColorInfo(
        "bt709", "bt709", "smpte170m", "limited"
    )
    assert yuyv_picture_color_of(
        ColorInfo(primaries="bt709", transfer="srgb", range="full")
    ) == ColorInfo("bt709", "srgb", "smpte170m", "full")


@pytest.mark.parametrize(
    "color, fields",
    [
        (
            ColorInfo("bt709", "srgb", "smpte170m", "full"),
            V4l2PixFormatColorFields(8, 2, 1, 1),
        ),
        (
            ColorInfo("bt709", "bt709", "bt709", "limited"),
            V4l2PixFormatColorFields(3, 1, 2, 2),
        ),
        (
            ColorInfo("bt2020", "smpte2084", "bt2020_ncl", "limited"),
            V4l2PixFormatColorFields(10, 7, 6, 2),
        ),
        (
            ColorInfo("bt2020", "arib_std_b67", "bt2020_cl", "full"),
            V4l2PixFormatColorFields(10, 0, 7, 1),
        ),
        (
            ColorInfo("smpte431", "linear", "smpte240m", "full"),
            V4l2PixFormatColorFields(12, 5, 8, 1),
        ),
        (
            ColorInfo("film", "gamma22", "ycgco", "limited"),
            V4l2PixFormatColorFields(0, 1, 0, 2),
        ),
    ],
)
def test_the_device_announces_the_colour_the_engine_converts_under(color, fields):
    assert v4l2_color_fields_of(color) == fields


def test_rows_copy_into_a_padded_device_buffer_and_in_one_move_when_both_are_packed():
    source = (ctypes.c_uint8 * 8)(*range(1, 9))
    padded = (ctypes.c_uint8 * 12)()
    copy_rows_between(
        source_address=ctypes.addressof(source),
        source_bytes_per_row=4,
        destination_address=ctypes.addressof(padded),
        destination_bytes_per_row=6,
        row_byte_count=4,
        row_count=2,
    )
    assert list(padded) == [1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0]

    packed = (ctypes.c_uint8 * 8)()
    copy_rows_between(
        source_address=ctypes.addressof(source),
        source_bytes_per_row=4,
        destination_address=ctypes.addressof(packed),
        destination_bytes_per_row=4,
        row_byte_count=4,
        row_count=2,
    )
    assert list(packed) == list(range(1, 9))


def test_the_distribution_lists_the_sink_in_the_node_catalog_entry_point_group():
    node_entry_points = importlib.metadata.entry_points(group="tatolab.nodes")
    virtual_camera_entry_point = next(
        entry_point
        for entry_point in node_entry_points
        if entry_point.name == "VirtualCameraSink"
    )

    assert virtual_camera_entry_point.load() is VirtualCameraSink
