# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only
# pyright: reportUnnecessaryTypeIgnoreComment=true

"""A read through a port attribute is typed by the port's family, as pyright reads it.

Pyright checks this file and reports an ignore comment that silences nothing,
so each `assert_type` proves a read's narrowing and the ignore on the Image
read used as a `ChannelsBlock` proves pyright refuses it.
"""

from __future__ import annotations

from typing import Any, assert_type

from tatolab.stream import (
    Array,
    ArrayFrame,
    Channels,
    ChannelsBlock,
    Image,
    ImageFrame,
    Input,
    LinkInputDataReader,
    LinkOutputDataWriter,
    Message,
    Output,
    Packets,
    PacketsUnit,
    PortPayloadBorrow,
    Records,
    node,
)


class EveryFamilyIn:
    video = Input(Image, delivery="newest")
    tensors = Input(Array, delivery="ordered")
    audio = Input(Channels, delivery="ordered")
    packets = Input(Packets, delivery="ordered")
    events = Input(Records, delivery="ordered")
    video_out = Output(Image)

    def read_each_family_through_its_attribute(self, inputs: LinkInputDataReader) -> None:
        assert_type(inputs.read(self.video), ImageFrame | None)
        assert_type(inputs.read(self.tensors), ArrayFrame | None)
        assert_type(inputs.read(self.audio), ChannelsBlock | None)
        assert_type(inputs.read(self.packets), PacketsUnit | None)
        assert_type(inputs.read(self.events), "dict[str, Any] | None")
        assert_type(inputs.read("video"), PortPayloadBorrow | None)
        assert_type(inputs.read_message(self.audio), Message[ChannelsBlock] | None)

    def read_an_image_port_as_a_channels_block(self, inputs: LinkInputDataReader) -> None:
        block: ChannelsBlock | None = inputs.read(self.video)  # pyright: ignore[reportAssignmentType]
        del block

    def write_an_image_frame_on(
        self, inputs: LinkInputDataReader, outputs: LinkOutputDataWriter
    ) -> None:
        frame = inputs.read(self.video)
        if frame is not None:
            outputs.write(self.video_out, frame)


def test_every_family_port_declares_under_its_attribute_name():
    declared = node.process(execution="manual")(EveryFamilyIn)
    assert [port["name"] for port in declared.__tatolab_node_input_ports__] == [
        "video",
        "tensors",
        "audio",
        "packets",
        "events",
    ]
