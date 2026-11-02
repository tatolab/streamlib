# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The proof's source node and the recorder its A/B harness measures both sources with.

`MicrophoneProofSource` runs in its own helper process like any Python
processor. Its native half owns the device: an ALSA thread reads whole periods
into a lock-free ring and encodes nothing; this node's one drain thread waits on
the ring with the GIL released and writes each block's ready-made message. So
Python runs once per block, never per sample.
"""

from __future__ import annotations

import dataclasses
import threading
import time
from typing import IO, Annotated

from tatolab.stream import (
    LinkOutputDataWriter,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    log,
    node,
)
from tatolab.stream.audio_block import AudioBlock
from tatolab.stream.catalog import BinaryEntry

from . import _native

AUDIO_PORT = "audio"

#: How long the drain waits on the ring before looking at its stop flag.
DRAIN_WAIT_TIMEOUT_MS = 100

#: Bounded well inside the helper's five-second teardown budget.
THREAD_JOIN_TIMEOUT_SECONDS = 1.0

#: Rows the recorder buffers between flushes: about a second of blocks.
RECORDED_ROWS_BETWEEN_FLUSHES = 50

RECORDING_COLUMNS = (
    "arrival_ns",
    "first_sample_timestamp_ns",
    "sample_count",
    "sample_rate",
    "lost_since_last_read",
)


@dataclasses.dataclass
class MicrophoneProofSourceConfig:
    """What a `MicrophoneProofSource` is configured with."""

    pcm_name: Annotated[
        str,
        "The ALSA PCM to capture. `default` reaches the audio server's default source; "
        "`plughw:<card>,<device>` the card directly, when no server holds it.",
    ] = "default"
    sample_rate: Annotated[int, "Hz."] = 48_000
    channels: Annotated[int, "Interleaved channel count."] = 1
    block_frames: Annotated[
        int, "Frames per published block; 1024 matches the built-in's PipeWire quantum."
    ] = 1024
    ring_capacity_blocks: Annotated[
        int, "Blocks the ring holds before it drops the oldest; the built-in holds 16."
    ] = 16
    gil_churn_threads: Annotated[
        int,
        "Pure-Python busy threads started beside the drain, contending for this "
        "interpreter's GIL. Stress only.",
    ] = 0


@node(
    execution="manual",
    scheduling="realtime",
    description="Captures audio from ALSA on a native thread in this node's helper process",
)
class MicrophoneProofSource:
    """ALSA capture in the helper, drained one whole block at a time.

    Publishes the built-in `MicrophoneSource`'s audio-block message on `audio`,
    stamped with the device instant of each block's first sample.
    """

    def __init__(self, config: MicrophoneProofSourceConfig) -> None:
        self._config = config
        self._capture: "_native.AlsaMicrophoneCapture | None" = None
        self._stop = threading.Event()
        self._drain: "threading.Thread | None" = None
        self._gil_churners: "list[threading.Thread]" = []
        self._written_blocks = 0
        self._failed_writes = 0
        self._drain_thread_priority = "not started"

    @node.output(catalog=BinaryEntry(mode="snapshot", mime="application/msgpack"))
    def audio(self) -> None:
        """Timestamped blocks of interleaved capture samples."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        config = self._config
        self._capture = _native.AlsaMicrophoneCapture(
            config.pcm_name,
            config.sample_rate,
            config.channels,
            config.block_frames,
            config.ring_capacity_blocks,
        )
        log.info(
            f"MicrophoneProofSource: opened {config.pcm_name!r} at "
            f"{self._capture.sample_rate} Hz × {self._capture.channels} ch, ALSA buffer "
            f"{self._capture.buffer_frames} frames, period {self._capture.period_frames}"
        )

    def start(self, ctx: RuntimeContextFullAccess) -> None:
        capture = self._opened_capture()
        outputs = ctx.outputs
        capture.start()
        self._drain = threading.Thread(
            target=self._drain_until_stopped,
            args=(capture, outputs),
            name="microphone-proof-drain",
            daemon=True,
        )
        self._drain.start()
        for index in range(self._config.gil_churn_threads):
            churner = threading.Thread(
                target=self._churn_the_gil_until_stopped,
                name=f"microphone-proof-gil-churn-{index}",
                daemon=True,
            )
            churner.start()
            self._gil_churners.append(churner)
        log.info(
            f"MicrophoneProofSource: capturing, gil_churn_threads="
            f"{self._config.gil_churn_threads}"
        )

    def _drain_until_stopped(
        self, capture: "_native.AlsaMicrophoneCapture", outputs: LinkOutputDataWriter
    ) -> None:
        # The built-in's publishing thread is realtime too; the drain is this node's.
        self._drain_thread_priority = _native.make_the_calling_thread_realtime()
        while True:
            message = capture.next_message(DRAIN_WAIT_TIMEOUT_MS)
            if message is None:
                if self._stop.is_set() or capture.capture_has_ended:
                    return
                continue
            try:
                outputs.write(
                    AUDIO_PORT, message.payload, timestamp_ns=message.first_sample_timestamp_ns
                )
            except Exception as write_failure:
                self._failed_writes += 1
                if self._failed_writes == 1:
                    log.error(f"MicrophoneProofSource: writing a block failed: {write_failure}")
                continue
            self._written_blocks += 1

    def _churn_the_gil_until_stopped(self) -> None:
        while not self._stop.is_set():
            sum(value * value for value in range(2_000))

    def stop(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._stop.set()
        capture = self._capture
        if capture is not None:
            capture.stop()
        for thread in [self._drain, *self._gil_churners]:
            if thread is not None:
                thread.join(timeout=THREAD_JOIN_TIMEOUT_SECONDS)
        self._drain = None
        self._gil_churners = []
        if capture is not None:
            log.info(
                f"MicrophoneProofSource: stopped. captured_blocks={capture.captured_block_count} "
                f"written_blocks={self._written_blocks} failed_writes={self._failed_writes} "
                f"ring_dropped_blocks={capture.ring_dropped_block_count} "
                f"device_overruns={capture.device_overrun_count} "
                f"device_thread_priority={capture.device_thread_priority!r} "
                f"drain_thread_priority={self._drain_thread_priority!r} "
                f"capture_failure={capture.failure_that_ended_the_capture}"
            )

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        self.stop(ctx)
        self._capture = None

    def _opened_capture(self) -> "_native.AlsaMicrophoneCapture":
        if self._capture is None:
            raise RuntimeError("MicrophoneProofSource: start() ran before setup() opened the device")
        return self._capture


@dataclasses.dataclass
class AudioBlockArrivalRecorderConfig:
    """What an `AudioBlockArrivalRecorder` is configured with."""

    recording_path: Annotated[
        str, "The CSV each arrival is appended to, one row per block, with a header."
    ]


@node(description="Records when each audio block arrives, for the microphone A/B harness")
class AudioBlockArrivalRecorder:
    """One CSV row per audio block: when it arrived beside what it says about itself."""

    def __init__(self, config: AudioBlockArrivalRecorderConfig) -> None:
        self._recording_path = config.recording_path
        self._recording: "IO[str] | None" = None
        self._rows_since_flush = 0

    @node.input(delivery_profile="ordered")
    def audio(self) -> None:
        """Audio blocks from the source under test."""

    def setup(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        self._recording = open(self._recording_path, "w", encoding="ascii")
        self._recording.write(",".join(RECORDING_COLUMNS) + "\n")

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        recording = self._recording
        if recording is None:
            return
        while True:
            message = ctx.inputs.read_message(AUDIO_PORT, into=AudioBlock)
            if message is None:
                return
            arrival_ns = time.monotonic_ns()
            block = message.payload
            recording.write(
                f"{arrival_ns},{block.first_sample_timestamp_ns},{block.sample_count},"
                f"{block.sample_rate},{message.lost_since_last_read}\n"
            )
            self._rows_since_flush += 1
            if self._rows_since_flush >= RECORDED_ROWS_BETWEEN_FLUSHES:
                recording.flush()
                self._rows_since_flush = 0

    def teardown(self, ctx: RuntimeContextFullAccess) -> None:
        del ctx
        if self._recording is not None:
            self._recording.close()
            self._recording = None
