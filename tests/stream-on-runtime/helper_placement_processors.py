# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Processors that report which process they are running in.

The test process imports this module to compile a stream; every other import
of it is a processor interpreter's, describing a class or running one. With
`STREAMLIB_TEST_MODULE_IMPORT_RECORD_DIRECTORY` set, each import records its
own pid and its parent's there — the witness that `tatolabd` never imported it.
"""

import ctypes
import dataclasses
import json
import os
import time

import numpy

from tatolab.stream import Channels, Input, Output, Packets, Records, State, Stateful, log, node

#: A payload no msgpack decoder reads: `0xc1` is the one type byte msgpack never
#: uses, then every byte value once, so an encode or decode anywhere between
#: the writer and the reader changes or refuses it.
RAW_MESSAGE_PAYLOAD_BYTES = bytes([0xC1]) + bytes(range(256))

#: Names the directory each import of this module records `<pid>` in, holding
#: the importing process's parent pid.
MODULE_IMPORT_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE = "STREAMLIB_TEST_MODULE_IMPORT_RECORD_DIRECTORY"

if MODULE_IMPORT_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE in os.environ:
    with open(
        os.path.join(os.environ[MODULE_IMPORT_RECORD_DIRECTORY_ENVIRONMENT_VARIABLE], str(os.getpid())),
        "w",
    ) as module_import_record:
        module_import_record.write(str(os.getppid()))


def report(marker_name: str, payload: "dict[str, object] | None" = None) -> None:
    log.info(f"MARKER:{marker_name}" + ("" if payload is None else " " + json.dumps(payload)))


@dataclasses.dataclass
class ReportsItsOwnProcessSourceConfig:
    label: str = "unlabelled"


@node.process(execution="continuous", interval_ms=10)
class ReportsItsOwnProcessSource:
    """Stamps every message with the pid it was produced in."""

    def __init__(self, config: ReportsItsOwnProcessSourceConfig) -> None:
        self.label = config.label
        self.announced = False

    frames_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        if not self.announced:
            report("SOURCE_PID", {"label": self.label, "pid": os.getpid()})
            self.announced = True
        ctx.outputs.write(self.frames_to_downstream, {"produced_in_pid": os.getpid()})


@node.process
class ReportsUpstreamProcessSink:
    """Announces the pid a message was produced in, alongside its own."""

    def __init__(self) -> None:
        self.messages_seen = 0

    frames_from_upstream = Input(Records, delivery="newest")

    def process(self, ctx) -> None:
        payload = ctx.inputs.read(self.frames_from_upstream)
        if payload is None:
            return
        # Announced on every tenth message rather than once: a test that waits
        # for this *after* another processor crashed needs evidence of live
        # traffic, not a marker emitted before the crash.
        self.messages_seen += 1
        if self.messages_seen % 10 == 1:
            upstream_pid = payload["produced_in_pid"]
            report("SINK_PID", {"sink_pid": os.getpid(), "upstream_pid": upstream_pid})


@node.process
class ReportsItsOwnProcessRecordsSink:
    """Announces its own process at setup, downstream of a native built-in.

    The records come from `ChannelsToRecords`, which is native and therefore
    has no process of its own — so this sink's pid, set against `tatolabd`'s,
    is what discriminates the two sides of the boundary.
    """

    records_from_upstream = Input(Records, delivery="newest")

    def setup(self, ctx) -> None:
        report("RECORDS_SINK_PID", {"pid": os.getpid()})

    def process(self, ctx) -> None:
        ctx.inputs.read("records_from_upstream")


@node.process(execution="continuous", interval_ms=10)
class WritesRawMessagePayloadSource:
    """Writes `RAW_MESSAGE_PAYLOAD_BYTES` every tick, after reporting once what
    handing `write` a dict raised."""

    def __init__(self) -> None:
        self.reported_the_dict_write = False

    raw_messages_to_downstream = Output(Packets)

    def process(self, ctx) -> None:
        if not self.reported_the_dict_write:
            self.reported_the_dict_write = True
            try:
                ctx.outputs.write("raw_messages_to_downstream", {"not": "bytes"})
            except Exception as refusal:  # noqa: BLE001 — the refusal is the observation
                refusal_type, refusal_text = type(refusal).__name__, str(refusal)
            else:
                refusal_type, refusal_text = None, None
            report(
                "DICT_WRITE_OUTCOME",
                {
                    "source_pid": os.getpid(),
                    "refusal_type": refusal_type,
                    "refusal_text": refusal_text,
                },
            )
        ctx.outputs.write("raw_messages_to_downstream", RAW_MESSAGE_PAYLOAD_BYTES)


@node.process
class ReportsTheRawMessagePayloadItReadSink:
    """Reports the first payload it reads, as hex, with its type and its own pid."""

    def __init__(self) -> None:
        self.reported = False

    raw_messages_from_upstream = Input(Packets, delivery="ordered")

    def process(self, ctx) -> None:
        payload = ctx.inputs.read("raw_messages_from_upstream")
        if payload is None or self.reported:
            return
        self.reported = True
        report(
            "RAW_MESSAGE_PAYLOAD_READ",
            {
                "sink_pid": os.getpid(),
                "payload_type": type(payload).__name__,
                "payload_hex": bytes(payload).hex(),
            },
        )


@node.process(execution="continuous", interval_ms=10)
class DiesAbruptlyProbe:
    """Takes its own process down mid-run, the way a segfaulting native call
    inside a user callback would."""

    def __init__(self) -> None:
        self.frames_before_dying = 3

    frames_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        self.frames_before_dying -= 1
        if self.frames_before_dying <= 0:
            report("ABOUT_TO_DIE", {"pid": os.getpid()})
            # Not an exception: the point is a process that stops existing
            # without unwinding, which is what a segfaulting native call does.
            os._exit(1)


@node.process(execution="continuous", interval_ms=10)
class SleepsThroughItsOwnShutdownProbe:
    """Parks in `process()` far past the ladder's one-second budget.

    The bag it had in flight is lost to the interrupt, and `stop()` and
    `teardown()` still run — which is what the markers below are for.
    """

    frames_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        report("ASLEEP_IN_PROCESS", {"pid": os.getpid()})
        time.sleep(30)
        report("SLEPT_THE_WHOLE_WAY")

    def stop(self, ctx) -> None:
        report("SLEEPER_STOPPED")

    def teardown(self, ctx) -> None:
        report("SLEEPER_TORE_DOWN")


@node.process(execution="continuous", interval_ms=50)
class ForksAWorkerThatOutlivesItProbe:
    """Starts a worker of its own the way `os.system("sleep 60 &")` does.

    The worker is the descendant the process-group rung exists to reach. Its
    pid is announced so a test can go looking for it after `tatolabd` is gone.
    """

    def __init__(self) -> None:
        self.worker_pid = None

    frames_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        if self.worker_pid is None:
            self.worker_pid = os.fork()
            if self.worker_pid == 0:
                time.sleep(120)
                os._exit(0)
            report("WORKER_PID", {"worker_pid": self.worker_pid, "helper_pid": os.getpid()})


@node.process(execution="manual")
class SleepsThroughItsOwnSetupProbe:
    """Parks in `setup()`, so shutdown finds it still registering.

    That is the one route onto the ladder the engine's `stop()` hook never
    reaches, and the plan still owes this processor its `teardown()`.
    """

    frames_to_downstream = Output(Records)

    def setup(self, ctx) -> None:
        report("ASLEEP_IN_SETUP", {"pid": os.getpid()})
        time.sleep(120)
        report("SLEPT_THE_WHOLE_SETUP")

    def teardown(self, ctx) -> None:
        report("INTERRUPTED_SETUP_TORE_DOWN")



#: How many reads of the function node island a sink makes before it reports them.
FUNCTION_NODE_ISLAND_READS_BEFORE_REPORTING = 20


@node.process(execution="continuous", interval_ms=10)
class ReportsTheFunctionNodeValuesItReadSink:
    """Reads the value a function node island feeds `brightness`, and reports
    the values of its first reads with its own pid."""

    def __init__(self) -> None:
        self.values_read: "list[float]" = []
        self.sample_rates_read: "set[int]" = set()
        self.reported_its_start = False

    brightness = Input(Channels, delivery="newest")

    def process(self, ctx) -> None:
        if not self.reported_its_start:
            self.reported_its_start = True
            report("FUNCTION_NODE_SINK_STARTED", {"pid": os.getpid()})
        if len(self.values_read) >= FUNCTION_NODE_ISLAND_READS_BEFORE_REPORTING:
            return
        block = ctx.inputs.read(self.brightness)
        if block is None:
            return
        self.sample_rates_read.add(block.sample_rate)
        self.values_read.append(float(numpy.from_dlpack(block)[0][0]))
        if len(self.values_read) == FUNCTION_NODE_ISLAND_READS_BEFORE_REPORTING:
            report(
                "FUNCTION_NODE_VALUES_READ",
                {
                    "sink_pid": os.getpid(),
                    "values": self.values_read,
                    "sample_rates": sorted(self.sample_rates_read),
                },
            )


#: How many reads a batch-reporting sink makes into each batch: past a second at
#: its interval, so a batch of a 1 Hz `Lfo` read directly spans -1..1.
FUNCTION_NODE_ISLAND_READS_PER_REPORTED_BATCH = 120


@node.process(execution="continuous", interval_ms=10)
class ReportsEachBatchOfFunctionNodeValuesItReadSink:
    """Reads the value an island feeds `brightness`, and reports the range of
    each batch of reads with its own pid and when the batch started."""

    def __init__(self) -> None:
        self.values_in_this_batch: "list[float]" = []
        self.this_batch_started_at_monotonic_ns = time.monotonic_ns()

    brightness = Input(Channels, delivery="newest")

    def process(self, ctx) -> None:
        block = ctx.inputs.read(self.brightness)
        if block is None:
            return
        if not self.values_in_this_batch:
            self.this_batch_started_at_monotonic_ns = time.monotonic_ns()
        self.values_in_this_batch.append(float(numpy.from_dlpack(block)[0][0]))
        if len(self.values_in_this_batch) == FUNCTION_NODE_ISLAND_READS_PER_REPORTED_BATCH:
            report(
                "FUNCTION_NODE_VALUE_BATCH_READ",
                {
                    "sink_pid": os.getpid(),
                    "started_at_monotonic_ns": self.this_batch_started_at_monotonic_ns,
                    "lowest": min(self.values_in_this_batch),
                    "highest": max(self.values_in_this_batch),
                },
            )
            self.values_in_this_batch = []


@dataclasses.dataclass
class ReportsEachBatchOfItsConfiguredLevelConfig:
    level: float = 0.25


@node.process(execution="continuous", interval_ms=10)
class ReportsEachBatchOfItsConfiguredLevel:
    """Reads the `level` of the config it was last handed at each run, a field a
    link can drive once it is exposed, and reports the range of each batch."""

    def __init__(self, config: ReportsEachBatchOfItsConfiguredLevelConfig) -> None:
        self.level = config.level
        self.levels_in_this_batch: "list[float]" = []
        self.this_batch_started_at_monotonic_ns = time.monotonic_ns()

    def configure(self, config: ReportsEachBatchOfItsConfiguredLevelConfig) -> None:
        self.level = config.level

    def process(self, ctx) -> None:
        if not self.levels_in_this_batch:
            self.this_batch_started_at_monotonic_ns = time.monotonic_ns()
        self.levels_in_this_batch.append(float(self.level))
        if len(self.levels_in_this_batch) == FUNCTION_NODE_ISLAND_READS_PER_REPORTED_BATCH:
            report(
                "FUNCTION_NODE_VALUE_BATCH_READ",
                {
                    "sink_pid": os.getpid(),
                    "started_at_monotonic_ns": self.this_batch_started_at_monotonic_ns,
                    "lowest": min(self.levels_in_this_batch),
                    "highest": max(self.levels_in_this_batch),
                },
            )
            self.levels_in_this_batch = []


@node.process(execution="continuous", interval_ms=10)
class ReportsEachBatchOfPinValuesItReadSink(ReportsEachBatchOfFunctionNodeValuesItReadSink):
    """Reads `brightness`, which nothing is linked into and which declares 0.25,
    and reports the range of each batch of reads."""

    brightness = Input(Channels, delivery="newest", default=0.25)


@node.function
def SpinsForever(start: float = 1.0) -> float:  # noqa: N802
    """Never returns: a compiled call no thread can pre-empt, which only the watchdog ends."""
    climbing = start
    while climbing > 0.0:
        climbing = climbing + 1.0
    return climbing


@dataclasses.dataclass(frozen=True)
class CallsCountedSoFar:
    calls: float = 0.0


@node.function
def CountsItsOwnCalls(  # noqa: N802
    step: float = 1.0, *, previous: State[CallsCountedSoFar]
) -> Stateful[float, CallsCountedSoFar]:
    """The number of times it has been called, carried between calls in its state."""
    calls = previous.calls + step
    return calls, CallsCountedSoFar(calls)


@dataclasses.dataclass(frozen=True)
class LevelScaledByItsGainConfig:
    gain: float = 0.25


@node.function
def LevelScaledByItsGain(  # noqa: N802
    level: float = 1.0, *, config: LevelScaledByItsGainConfig
) -> float:
    """Its level scaled by the `gain` of its config, a field a link can drive once it is exposed."""
    return level * config.gain


@dataclasses.dataclass(frozen=True)
class HeldLevelConfig:
    level: float = 0.25


@node.function
def HoldsItsConfiguredLevelCountingItsCalls(  # noqa: N802
    step: float = 1.0, *, config: HeldLevelConfig, previous: State[CallsCountedSoFar]
) -> Stateful[float, CallsCountedSoFar]:
    """The `level` of its config, a field a link can drive once it is exposed;
    stateful, so two readers move it into a helper of its own."""
    return config.level, CallsCountedSoFar(previous.calls + step)


#: The count at which a sink whose count started fresh takes its own helper down.
COUNT_AT_WHICH_A_FRESH_COUNT_SINK_DIES = 20.0


@node.process(execution="continuous", interval_ms=10)
class ReportsTheCountItReadAndDiesOnceAFreshCountReachesTwenty:
    """Reports the first count it reads with its own pid, and takes its own
    helper down once a count that started at one reaches twenty. A count it
    first reads above one was carried over, so it reads on without dying."""

    def __init__(self) -> None:
        self.first_count_read: "float | None" = None

    count = Input(Channels, delivery="newest")

    def process(self, ctx) -> None:
        block = ctx.inputs.read(self.count)
        if block is None:
            return
        count_read = float(numpy.from_dlpack(block)[0][0])
        if self.first_count_read is None:
            self.first_count_read = count_read
            report("FIRST_COUNT_READ", {"pid": os.getpid(), "count": count_read})
        if self.first_count_read <= 1.0 and count_read >= COUNT_AT_WHICH_A_FRESH_COUNT_SINK_DIES:
            report("FRESH_COUNT_SINK_ABOUT_TO_DIE", {"pid": os.getpid(), "count": count_read})
            os._exit(1)


#: The stream milliseconds between the steps of the stepped LFO stream.
STEPPED_LFO_STEP_MS = 100

#: Steps of the stepped LFO stream a sink reads before it reports them.
STEPPED_LFO_STEPS_REPORTED = 12


@node.process(execution="continuous", interval_ms=STEPPED_LFO_STEP_MS)
class ReportsTheLfoValueOfEachStepSink:
    """Reads the value an `Lfo` island evaluates to at each step, and reports
    the first `STEPPED_LFO_STEPS_REPORTED` with their step indices."""

    def __init__(self) -> None:
        self.value_read_at_each_step: "dict[int, float]" = {}

    wave = Input(Channels, delivery="newest")

    def process(self, ctx) -> None:
        step_index = ctx.stream_clock.step_index
        if step_index in self.value_read_at_each_step:
            return
        if len(self.value_read_at_each_step) >= STEPPED_LFO_STEPS_REPORTED:
            return
        block = ctx.inputs.read(self.wave)
        if block is None:
            return
        self.value_read_at_each_step[step_index] = float(numpy.from_dlpack(block)[0][0])
        if len(self.value_read_at_each_step) == STEPPED_LFO_STEPS_REPORTED:
            report(
                "STEPPED_LFO_VALUES_READ",
                {
                    "pid": os.getpid(),
                    "steps": [[step, value] for step, value in sorted(self.value_read_at_each_step.items())],
                },
            )


#: How many counts a sink of a counter several readers read reads before it reports them.
COUNTS_READ_IN_ORDER_BEFORE_REPORTING = 20


@node.process(execution="continuous", interval_ms=10)
class ReportsTheCountsItReadInOrderSink:
    """Reads every count written on `count`, in order, and reports the first
    `COUNTS_READ_IN_ORDER_BEFORE_REPORTING` with its own pid."""

    def __init__(self) -> None:
        self.counts_read: "list[float]" = []

    count = Input(Channels, delivery="ordered")

    def process(self, ctx) -> None:
        while len(self.counts_read) < COUNTS_READ_IN_ORDER_BEFORE_REPORTING:
            block = ctx.inputs.read(self.count)
            if block is None:
                return
            self.counts_read.append(float(numpy.from_dlpack(block)[0][0]))
            if len(self.counts_read) == COUNTS_READ_IN_ORDER_BEFORE_REPORTING:
                report("COUNTS_READ_IN_ORDER", {"pid": os.getpid(), "counts": self.counts_read})


#: How many records of a function node run as Python a sink reads before it reports them.
RECORDS_READS_BEFORE_REPORTING = 10


@node.function
def LabelsTheLevel(level: float = 0.75) -> dict:  # noqa: N802
    """Names the level it is handed: a body taking a number and giving a `dict`, which runs as Python."""
    return {"label": "high" if level > 0.5 else "low", "spelled": f"{level:.2f}"}


@node.process(execution="continuous", interval_ms=10)
class ReportsTheRecordsItReadSink:
    """Reads the record a function node island feeds `label`, and reports its
    first records with its own pid."""

    def __init__(self) -> None:
        self.records_read: "list[dict]" = []
        self.reported_its_start = False

    label = Input(Records, delivery="newest")

    def process(self, ctx) -> None:
        if not self.reported_its_start:
            self.reported_its_start = True
            report("RECORDS_SINK_STARTED", {"pid": os.getpid()})
        if len(self.records_read) >= RECORDS_READS_BEFORE_REPORTING:
            return
        record = ctx.inputs.read(self.label)
        if record is None:
            return
        self.records_read.append(record)
        if len(self.records_read) == RECORDS_READS_BEFORE_REPORTING:
            report("RECORDS_READ", {"sink_pid": os.getpid(), "records": self.records_read})


@node.function
def CrashesItsHelper(start: float = 1.0) -> float:  # noqa: N802
    """Reads address zero, which no compiled body can do, so it runs as Python and takes its helper down with SIGSEGV."""
    ctypes.string_at(0)
    return start


#: How many records of a pooled island's level a sink reads before it reports them.
LEVEL_RECORDS_READ_BEFORE_REPORTING = 10


@node.process(execution="continuous", interval_ms=10)
class ReportsTheLevelRecordsItReadSink:
    """Reads the records a native converter makes of a pooled island's value,
    and reports its first records with its own pid."""

    def __init__(self) -> None:
        self.records_read: "list[dict]" = []

    level_records = Input(Records, delivery="newest")

    def process(self, ctx) -> None:
        if len(self.records_read) >= LEVEL_RECORDS_READ_BEFORE_REPORTING:
            return
        record = ctx.inputs.read(self.level_records)
        if record is None:
            return
        self.records_read.append(record)
        if len(self.records_read) == LEVEL_RECORDS_READ_BEFORE_REPORTING:
            report("LEVEL_RECORDS_READ", {"sink_pid": os.getpid(), "records": self.records_read})


@node.process(execution="continuous", interval_ms=10)
class CountsEachTickRecordsSource:
    """Writes `{"count": n}` every tick, counting from 1."""

    def __init__(self) -> None:
        self.count = 0

    counts_to_downstream = Output(Records)

    def process(self, ctx) -> None:
        self.count += 1
        ctx.outputs.write(self.counts_to_downstream, {"count": self.count})


#: How many reads a gated sink makes into each batch it reports.
GATED_READS_PER_REPORTED_BATCH = 50


@node.process(execution="continuous", interval_ms=10)
class ReportsEachBatchOfGatedCountsItReadSink:
    """Reads the counts a `Gate` hands on, and reports each batch of reads with
    its own pid and when the batch started: the counts it read, and how many
    reads found nothing."""

    def __init__(self) -> None:
        self.counts_in_this_batch: "list[int]" = []
        self.reads_in_this_batch = 0
        self.this_batch_started_at_monotonic_ns = time.monotonic_ns()

    gated_counts = Input(Records, delivery="newest")

    def process(self, ctx) -> None:
        if self.reads_in_this_batch == 0:
            self.this_batch_started_at_monotonic_ns = time.monotonic_ns()
        record = ctx.inputs.read(self.gated_counts)
        self.reads_in_this_batch += 1
        if record is not None:
            self.counts_in_this_batch.append(int(record["count"]))
        if self.reads_in_this_batch == GATED_READS_PER_REPORTED_BATCH:
            report(
                "GATED_COUNT_BATCH_READ",
                {
                    "sink_pid": os.getpid(),
                    "started_at_monotonic_ns": self.this_batch_started_at_monotonic_ns,
                    "counts": self.counts_in_this_batch,
                },
            )
            self.counts_in_this_batch = []
            self.reads_in_this_batch = 0
