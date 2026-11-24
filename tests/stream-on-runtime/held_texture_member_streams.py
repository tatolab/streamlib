# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` function `test_held_texture_member.py` runs from the suite project."""

from tatolab.stream import (
    BreakBundle,
    MakeBundle,
    PinTypeChoice,
    StreamBuilder,
    graph_generator,
)

from held_texture_member_nodes import (
    LetsEachFrameGo,
    MeteredPicture,
    PublishesOnePictureThenRefillsItsRing,
    ReportsThePixelsOfEachPictureItReads,
    WritesALevelEveryTick,
)


@graph_generator
def one_picture_held_in_a_member_while_its_ring_turns_over(
    stream_builder: StreamBuilder,
) -> None:
    """One picture wired into `MakeBundle`'s `picture` member, a level wired into its `level`
    member every tick, and the ring the picture came from turned over into a sink; the picture
    taken off each bundle by `BreakBundle` goes to a reporter that reads its pixels."""
    pictures = stream_builder.add(
        PublishesOnePictureThenRefillsItsRing, name="pictures"
    )
    levels = stream_builder.add(WritesALevelEveryTick, name="levels")
    churn_sink = stream_builder.add(LetsEachFrameGo, name="churn-sink")
    make = stream_builder.add(
        MakeBundle,
        name="make",
        input_values={"bundle_type": PinTypeChoice(MeteredPicture)},
    )
    take_apart = stream_builder.add(
        BreakBundle,
        name="take-apart",
        input_values={"bundle_type": PinTypeChoice(MeteredPicture)},
    )
    reporter = stream_builder.add(ReportsThePixelsOfEachPictureItReads, name="reporter")
    stream_builder.connect(
        pictures.output("picture"), make.input("members", member="picture")
    )
    stream_builder.connect(pictures.output("churn"), churn_sink.input("frames"))
    stream_builder.connect(
        levels.output("level"), make.input("members", member="level")
    )
    stream_builder.connect(make.output("bundle"), take_apart.input("bundle"))
    stream_builder.connect(
        take_apart.output("members", member="picture"), reporter.input("picture")
    )
