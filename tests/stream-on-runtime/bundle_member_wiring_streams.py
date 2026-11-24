# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only

"""The `@graph_generator` functions `test_bundle_member_wiring.py` runs from the suite project."""

from tatolab.stream import BreakBundle, MakeBundle, PinTypeChoice, StreamBuilder, graph_generator
from tatolab.testpattern import TestPatternSource

from bundle_member_wiring_nodes import (
    MeteredPicture,
    Reading,
    ReportsTheMembersItReads,
    ReportsThePictureAndTheLevelItReads,
    WritesOneLevelSource,
)

#: The size of the picture the test pattern draws.
TEST_PATTERN_WIDTH = 640
TEST_PATTERN_HEIGHT = 480

#: The label set on `MakeBundle`'s `members`, which no wire reaches.
LABEL_SET_ON_MAKE_BUNDLE = "set-on-make"

#: The level set on `MakeBundle`'s `members` beside a wired picture, which no wire reaches.
LEVEL_SET_BESIDE_THE_PICTURE = 0.625


@graph_generator
def a_number_into_one_member_and_each_member_out(stream_builder: StreamBuilder) -> None:
    """A level wired into `MakeBundle`'s `level` member, its `label` set, the bundle through
    `BreakBundle` and each member wired out of it into one reporter."""
    source = stream_builder.add(WritesOneLevelSource, name="source")
    make = stream_builder.add(
        MakeBundle,
        name="make",
        input_values={
            "bundle_type": PinTypeChoice(Reading),
            "members": Reading(level=0.0, label=LABEL_SET_ON_MAKE_BUNDLE),
        },
    )
    take_apart = stream_builder.add(
        BreakBundle, name="take-apart", input_values={"bundle_type": PinTypeChoice(Reading)}
    )
    reporter = stream_builder.add(ReportsTheMembersItReads, name="reporter")
    stream_builder.connect(source.output("level"), make.input("members", member="level"))
    stream_builder.connect(make.output("bundle"), take_apart.input("bundle"))
    stream_builder.connect(
        take_apart.output("members", member="level"), reporter.input("level")
    )
    stream_builder.connect(
        take_apart.output("members", member="label"), reporter.input("label")
    )


@graph_generator
def a_picture_and_a_number_into_members_and_each_member_out(stream_builder: StreamBuilder) -> None:
    """The test pattern wired into `MakeBundle`'s `picture` member and a level into its `level`
    member, the bundle through `BreakBundle` and each member wired out of it into one reporter."""
    pattern = stream_builder.add(
        TestPatternSource,
        name="pattern",
        input_values={"width": TEST_PATTERN_WIDTH, "height": TEST_PATTERN_HEIGHT},
    )
    source = stream_builder.add(WritesOneLevelSource, name="source")
    make = stream_builder.add(
        MakeBundle, name="make", input_values={"bundle_type": PinTypeChoice(MeteredPicture)}
    )
    take_apart = stream_builder.add(
        BreakBundle, name="take-apart", input_values={"bundle_type": PinTypeChoice(MeteredPicture)}
    )
    reporter = stream_builder.add(ReportsThePictureAndTheLevelItReads, name="reporter")
    stream_builder.connect(pattern.output("video"), make.input("members", member="picture"))
    stream_builder.connect(source.output("level"), make.input("members", member="level"))
    stream_builder.connect(make.output("bundle"), take_apart.input("bundle"))
    stream_builder.connect(
        take_apart.output("members", member="picture"), reporter.input("picture")
    )
    stream_builder.connect(take_apart.output("members", member="level"), reporter.input("level"))


@graph_generator
def a_picture_wired_and_a_number_set_into_members_and_each_member_out(
    stream_builder: StreamBuilder,
) -> None:
    """The test pattern wired into `MakeBundle`'s `picture` member and its `level` member set,
    reached by no wire, the bundle through `BreakBundle` and each member wired out of it into
    one reporter."""
    pattern = stream_builder.add(
        TestPatternSource,
        name="pattern",
        input_values={"width": TEST_PATTERN_WIDTH, "height": TEST_PATTERN_HEIGHT},
    )
    make = stream_builder.add(
        MakeBundle,
        name="make",
        input_values={
            "bundle_type": PinTypeChoice(MeteredPicture),
            "members": {"level": LEVEL_SET_BESIDE_THE_PICTURE},
        },
    )
    take_apart = stream_builder.add(
        BreakBundle, name="take-apart", input_values={"bundle_type": PinTypeChoice(MeteredPicture)}
    )
    reporter = stream_builder.add(ReportsThePictureAndTheLevelItReads, name="reporter")
    stream_builder.connect(pattern.output("video"), make.input("members", member="picture"))
    stream_builder.connect(make.output("bundle"), take_apart.input("bundle"))
    stream_builder.connect(
        take_apart.output("members", member="picture"), reporter.input("picture")
    )
    stream_builder.connect(take_apart.output("members", member="level"), reporter.input("level"))
