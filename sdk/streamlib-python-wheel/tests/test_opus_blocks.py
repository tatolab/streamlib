# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The Opus codec pair, marker class to decoded audio block.

The marker tests are pure Python. The graph tests boot a real engine, so they
carry `requires_gpu` like every other graph test here and run nowhere in CI —
libopus needs no device, but `Runtime` needs a GPU context.

No microphone: a Python source publishes a stereo tone at a stated rate, so
the channel count the encoder follows and the rate the decoder reconstructs at
are the test's own facts rather than the machine's.

The encoded-channel test is where the cast meets the engine: what
`EncodedAudioPacket` says an encoded bag is, asserted against bags libopus
actually wrote. Its GPU-free half — the wire keys, the refusals, the payload's
msgpack type — is `test_encoded_audio_packet_cast.py`.
"""

import json
import re
from pathlib import Path

import pytest

import streamlib
from streamlib import OpusDecoder, OpusEncoder
from opus_blocks_probes import (
    DECODED_BLOCKS_REPORTED,
    ENCODED_PACKETS_REPORTED,
    SOURCE_CHANNELS,
)

OPUS_BLOCKS_APP = Path(__file__).parent / "opus_blocks_app.py"

ENCODED_PACKET = re.compile(r"MARKER:ENCODED_PACKET (\{.*\})")
DECODED_BLOCK = re.compile(r"MARKER:DECODED_BLOCK (\{.*\})")

TWO_OPUS_MARKERS = [OpusEncoder, OpusDecoder]

# The framing the encoder's own window contract fixes: 20 ms at Opus's 48 kHz
# clock. Every packet spans exactly this many per-channel samples.
SAMPLES_IN_ONE_OPUS_PACKET = 960
NANOSECONDS_PER_OPUS_PACKET = SAMPLES_IN_ONE_OPUS_PACKET * 1_000_000_000 // 48_000

# libopus's lookahead at 48 kHz is `Fs/400 + Fs/250` = 312 samples, and 120 at
# `lowdelay`. The assertions read `pre_skip` off the bag rather than assuming
# either — what is under test is that the decoder trims exactly what the
# encoder reported, not what this file believes libopus reports.
PRE_SKIP_SAMPLES_A_CREDIBLE_ENCODER_REPORTS = range(1, SAMPLES_IN_ONE_OPUS_PACKET)

# How much of the decoded report has to pair with the encoded one for the trim
# assertion to be about the stream rather than one block. The two probes are
# separate helper processes attaching at their own pace, so this is the floor
# under the overlap, not the expectation.
DECODED_BLOCKS_TO_CROSS_CHECK = DECODED_BLOCKS_REPORTED // 2


# ---- marker semantics (no GPU) ---------------------------------------------


@pytest.mark.parametrize("marker_class", TWO_OPUS_MARKERS)
def test_the_marker_class_cannot_be_instantiated(marker_class):
    with pytest.raises(TypeError):
        marker_class()


@pytest.mark.parametrize("marker_class", TWO_OPUS_MARKERS)
def test_display_name_defaults_to_the_type_name(marker_class):
    runtime = streamlib.Runtime()
    try:
        block = runtime.add(marker_class)
        assert block.display_name == marker_class.__name__
    finally:
        runtime.shutdown()


def test_the_round_trip_wires_without_an_adapter():
    """Source into encoder, encoder into decoder — the port names compose as
    published, which is what makes three `rt.add` calls and two `rt.connect`
    calls the whole of an audio codec round trip. No rechunker between the
    source and the encoder: the encoder's own window contract frames."""
    runtime = streamlib.Runtime()
    try:
        microphone = runtime.add(streamlib.MicrophoneSource)
        encoder = runtime.add(OpusEncoder)
        decoder = runtime.add(OpusDecoder)
        speaker = runtime.add(streamlib.SpeakerSink)
        runtime.connect(microphone.output("audio"), encoder.input("audio"))
        runtime.connect(encoder.output("encoded_audio"), decoder.input("encoded_audio"))
        runtime.connect(decoder.output("audio"), speaker.input("audio"))
    finally:
        runtime.shutdown()


# ---- the round trip in a real graph (GPU) ----------------------------------


def _reported(pattern: "re.Pattern[str]", app_output: str) -> "list[dict]":
    """Every report the probe admitted, in the order it admitted them."""
    return [json.loads(report) for report in pattern.findall(app_output)]


@pytest.mark.requires_gpu
def test_the_encoded_channel_casts_and_carries_the_ordering_contract(
    start_app_under_test,
):
    """The encoded-domain link, read from Python: every bag libopus produced
    casts to an `EncodedAudioPacket`, and what the cast then reports is the
    wire contract the plan fixed.

    Every Opus packet is a sync point, so unlike the video probe this one
    enters at the first bag it sees and the ordering assertion is the whole
    of the doctrine: a `sequence_index` step other than exactly one is loss,
    and each packet is its own group.
    """
    app = start_app_under_test(OPUS_BLOCKS_APP)
    app.await_marker("EVERY_PROCESSOR_RUNNING")
    app.await_marker("ENCODED_PACKETS_COMPLETE")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    packets = _reported(ENCODED_PACKET, app.output)
    assert len(packets) == ENCODED_PACKETS_REPORTED, (
        f"the probe reported {len(packets)} packets, not "
        f"{ENCODED_PACKETS_REPORTED}; output:\n{app.output}"
    )

    for packet in packets:
        assert packet["codec"] == "opus", (
            "the bag names the elementary stream its bitstream actually is"
        )
        assert packet["is_sync_point"] is True, (
            "a decoder enters an Opus stream at any packet, so the flag is a "
            "constant of the convention"
        )
        assert packet["sample_rate"] == 48_000, (
            "Opus codes at its own clock whatever the source was resampled from"
        )
        assert packet["channels"] == SOURCE_CHANNELS, (
            "the encoder declares no channel count, so the packet carries the "
            "source's own"
        )
        assert packet["sample_count"] == SAMPLES_IN_ONE_OPUS_PACKET, (
            "the input port's window contract frames at 20 ms, so every packet "
            "spans 960 per-channel samples"
        )
        assert packet["pre_skip"] in PRE_SKIP_SAMPLES_A_CREDIBLE_ENCODER_REPORTS, (
            "`pre_skip` is the minted encoder's reported lookahead — a zero "
            f"would mean it was never asked, and {packet['pre_skip']} past a "
            "packet is not a lookahead"
        )
        assert packet["byte_count"] > 0, "a packet with no bytes decodes to nothing"

    for earlier, later in zip(packets, packets[1:]):
        assert later["sequence_index"] == earlier["sequence_index"] + 1, (
            "`sequence_index` is monotonic in publication order and never "
            f"resets, so the step {earlier['sequence_index']} → "
            f"{later['sequence_index']} is loss on the link"
        )
        assert later["group_index"] == earlier["group_index"] + 1, (
            "every packet is a sync point, so every packet is its own group"
        )


@pytest.mark.requires_gpu
def test_the_decoded_blocks_are_one_per_packet_and_stamped_a_lookahead_earlier(
    start_app_under_test,
):
    """The far side of the pair: what libopus reconstructed, read back as
    ordinary audio blocks.

    One block per packet and no re-framing — 960 samples every time, 20 ms
    apart, nothing held back — and each block stamped exactly `pre_skip`
    samples *earlier* than the packet whose audio it carries. That offset is
    the trim, observable from anywhere in the stream: a decoder that did not
    trim would emit each block at its packet's own stamp and so run a
    lookahead late against the audio it holds, which on a recording is the
    audio drifting against the video.

    The *entry* block — short by exactly `pre_skip`, stamped at the anchoring
    packet's instant — is not assertable here and is not this test's job. Both
    probes are helper processes that attach at their own pace, well after the
    decoder entered the stream, so the entry block is already gone by the time
    either exists. The engine test owns it, driving the decode body with no
    `Runtime` at all:
    `encoded_packet_to_audio_block_decoder.rs::a_later_blocks_derived_stamp_lands_on_the_stamp_of_the_packet_whose_input_it_carries`.
    """
    app = start_app_under_test(OPUS_BLOCKS_APP)
    app.await_marker("EVERY_PROCESSOR_RUNNING")
    app.await_every_marker("ENCODED_PACKETS_COMPLETE", "DECODED_BLOCKS_COMPLETE")
    app.interrupt()
    app.await_marker("CLEAN_EXIT")
    app.await_clean_exit()

    blocks = _reported(DECODED_BLOCK, app.output)
    assert len(blocks) == DECODED_BLOCKS_REPORTED, (
        f"the probe reported {len(blocks)} blocks, not "
        f"{DECODED_BLOCKS_REPORTED}; output:\n{app.output}"
    )
    packets = _reported(ENCODED_PACKET, app.output)
    assert packets, f"the encoded link reported nothing to compare against:\n{app.output}"

    for block in blocks:
        assert block["sample_rate"] == 48_000, (
            "a decoder reconstructs at Opus's own clock whatever the source "
            "was resampled from"
        )
        assert block["channels"] == SOURCE_CHANNELS, (
            "the decoded block carries the packet's own channel count, which "
            "followed the source's"
        )
        assert block["dtype"] == "f32", "libopus reconstructs float samples"
        assert block["sample_count"] == SAMPLES_IN_ONE_OPUS_PACKET, (
            "one block per packet and no re-framing — a decoder that re-framed "
            "would be a second framing system beside the window stage that "
            "already owns the concern"
        )
        assert block["sample_count"] * block["channels"] == block["scalars_read"], (
            "the block's declared count and the samples it actually carries "
            "have to be the same fact"
        )

    for earlier, later in zip(blocks, blocks[1:]):
        assert (
            later["timestamp_ns"] - earlier["timestamp_ns"]
        ) == NANOSECONDS_PER_OPUS_PACKET, (
            "the stamps are exactly 20 ms apart, derived from the run's anchor "
            "in integer rational arithmetic rather than read off a clock: "
            f"{earlier['timestamp_ns']} → {later['timestamp_ns']}"
        )

    # One minted encoder for the whole run — the channel count never changes,
    # so nothing re-mints and there is one lookahead to reason about.
    reported_lookaheads = {packet["pre_skip"] for packet in packets}
    assert len(reported_lookaheads) == 1, (
        f"the run reported {reported_lookaheads} lookaheads; a second one means "
        "the encoder re-minted, which nothing in this graph asks it to do"
    )
    trim_ns = reported_lookaheads.pop() * 1_000_000_000 // 48_000

    # Bounded by the encoded probe's own report: the two probes attach
    # independently, and a decoded block whose packet fell outside that window
    # rode a bag nobody wrote down.
    packet_stamps = {packet["timestamp_ns"] for packet in packets}
    paired = [
        block for block in blocks if block["timestamp_ns"] + trim_ns in packet_stamps
    ]
    assert len(paired) >= DECODED_BLOCKS_TO_CROSS_CHECK, (
        f"only {len(paired)} of {len(blocks)} decoded blocks paired with an "
        "encoded packet a lookahead later, which is too few to be about the "
        f"stream; output:\n{app.output}"
    )

    # The un-trimmed signature, and why the pairing above is the trim rather
    # than an arbitrary offset that happened to fit: packets are 20 ms apart
    # and the lookahead is a fraction of that, so a block stamped at any
    # packet's own instant is a decoder that emitted its priming.
    assert not [
        block for block in blocks if block["timestamp_ns"] in packet_stamps
    ], (
        "a decoded block is stamped at its packet's own instant, so the "
        "encoder's priming was never discarded — every block then holds audio "
        "a lookahead older than the moment it claims"
    )
