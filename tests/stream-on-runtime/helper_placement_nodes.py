# Copyright (c) 2025 Tatolab LLC
# SPDX-License-Identifier: GPL-3.0-only


import ctypes
import dataclasses
import json
import os
import time
from typing import Annotated, Callable

from tatolab.stream import (
    EncodedVideo,
    Input,
    InputPin,
    OutputPin,
    RuntimeContextFullAccess,
    RuntimeContextLimitedAccess,
    SettingOnly,
    State,
    Stateful,
    StreamDelivery,
    Trigger,
    log,
    node,
)

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


@dataclasses.dataclass(frozen=True)
class ProducedIn:
    """The pid a message was produced in."""

    produced_in_pid: int


@node(execution="continuous", interval_ms=10)
def ReportsItsOwnProcessSource(
    *,
    label: Input[str] = Input("unlabelled"),
    frames_to_downstream: OutputPin[ProducedIn],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Stamps every message with the pid it was produced in."""

    announced = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal announced
        if not announced:
            report("SOURCE_PID", {"label": label.get(), "pid": os.getpid()})
            announced = True
        frames_to_downstream.write(ProducedIn(produced_in_pid=os.getpid()))

    return each_frame


@node(execution="reactive")
def ReportsUpstreamProcessSink(
    *,
    frames_from_upstream: InputPin[Annotated[ProducedIn, StreamDelivery("newest")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Announces the pid a message was produced in, alongside its own."""

    messages_seen = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal messages_seen
        produced_in = frames_from_upstream.read()
        if produced_in is None:
            return
        messages_seen += 1
        if messages_seen % 10 == 1:
            report(
                "SINK_PID",
                {"sink_pid": os.getpid(), "upstream_pid": produced_in.produced_in_pid},
            )

    return each_frame


@node(execution="reactive")
def ReportsItsOwnProcessIntegerSink(
    *,
    integers_from_upstream: InputPin[Annotated[int, StreamDelivery("newest")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Announces its own process at setup, downstream of a native built-in.

    The integers come from `FloatToInteger`, which is native and therefore
    has no process of its own — so this sink's pid, set against `tatolabd`'s,
    is what discriminates the two sides of the boundary.
    """

    report("INTEGER_SINK_PID", {"pid": os.getpid()})

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        integers_from_upstream.read()

    return each_frame


@node(execution="continuous", interval_ms=10)
def WritesRawMessagePayloadSource(
    *,
    raw_messages_to_downstream: OutputPin[EncodedVideo],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes `RAW_MESSAGE_PAYLOAD_BYTES` every tick, after reporting once what
    handing `write` a dict raised."""

    reported_the_dict_write = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported_the_dict_write
        if not reported_the_dict_write:
            reported_the_dict_write = True
            try:
                raw_messages_to_downstream.write({"not": "bytes"})
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
        raw_messages_to_downstream.write(RAW_MESSAGE_PAYLOAD_BYTES)

    return each_frame


@node(execution="reactive")
def ReportsTheRawMessagePayloadItReadSink(
    *,
    raw_messages_from_upstream: InputPin[Annotated[EncodedVideo, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the first payload it reads, as hex, with its type and its own pid."""

    reported = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported
        unit = raw_messages_from_upstream.read()
        if unit is None or reported:
            return
        reported = True
        report(
            "RAW_MESSAGE_PAYLOAD_READ",
            {
                "sink_pid": os.getpid(),
                "payload_type": type(unit.payload).__name__,
                "payload_hex": bytes(unit.payload).hex(),
            },
        )

    return each_frame


@node(execution="continuous", interval_ms=10)
def DiesAbruptlyProbe(
    *,
    frames_to_downstream: OutputPin[Trigger],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Takes its own process down mid-run, the way a segfaulting native call
    inside a user callback would."""

    frames_before_dying = 3

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal frames_before_dying
        frames_before_dying -= 1
        if frames_before_dying <= 0:
            report("ABOUT_TO_DIE", {"pid": os.getpid()})
            # Not an exception: the point is a process that stops existing
            # without unwinding, which is what a segfaulting native call does.
            os._exit(1)

    return each_frame


@node(execution="continuous", interval_ms=10)
class SleepsThroughItsOwnShutdownProbe:
    """Parks in `process()` far past the ladder's one-second budget.

    The bag it had in flight is lost to the interrupt, and `stop()` and
    `teardown()` still run — which is what the markers below are for.
    """

    frames_to_downstream: OutputPin[Trigger]

    def process(self, ctx) -> None:
        report("ASLEEP_IN_PROCESS", {"pid": os.getpid()})
        time.sleep(30)
        report("SLEPT_THE_WHOLE_WAY")

    def stop(self, ctx) -> None:
        report("SLEEPER_STOPPED")

    def teardown(self, ctx) -> None:
        report("SLEEPER_TORE_DOWN")


@node(execution="continuous", interval_ms=50)
def ForksAWorkerThatOutlivesItProbe(
    *,
    frames_to_downstream: OutputPin[Trigger],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Starts a worker of its own the way `os.system("sleep 60 &")` does.

    The worker is the descendant the process-group rung exists to reach. Its
    pid is announced so a test can go looking for it after `tatolabd` is gone.
    """

    worker_pid = None

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal worker_pid
        if worker_pid is None:
            worker_pid = os.fork()
            if worker_pid == 0:
                time.sleep(120)
                os._exit(0)
            report("WORKER_PID", {"worker_pid": worker_pid, "helper_pid": os.getpid()})

    return each_frame


@node(execution="manual")
class SleepsThroughItsOwnSetupProbe:

    frames_to_downstream: OutputPin[Trigger]

    def setup(self, ctx) -> None:
        report("ASLEEP_IN_SETUP", {"pid": os.getpid()})
        time.sleep(120)
        report("SLEPT_THE_WHOLE_SETUP")

    def teardown(self, ctx) -> None:
        report("INTERRUPTED_SETUP_TORE_DOWN")



#: How many reads of the function node island a sink makes before it reports them.
FUNCTION_NODE_ISLAND_READS_BEFORE_REPORTING = 20


@node(execution="continuous", interval_ms=10)
def ReportsTheFunctionNodeValuesItReadSink(
    *,
    brightness: Input[float | None] = Input(None),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads the value a function node island feeds `brightness`, and reports
    the values of its first reads with its own pid."""

    values_read: "list[float]" = []

    report("FUNCTION_NODE_SINK_STARTED", {"pid": os.getpid()})

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        if len(values_read) >= FUNCTION_NODE_ISLAND_READS_BEFORE_REPORTING:
            return
        if brightness.get() is None:
            return
        values_read.append(brightness.get())
        if len(values_read) == FUNCTION_NODE_ISLAND_READS_BEFORE_REPORTING:
            report(
                "FUNCTION_NODE_VALUES_READ",
                {"sink_pid": os.getpid(), "values": values_read},
            )

    return each_frame


#: How many reads a batch-reporting sink makes into each batch: past a second at
#: its interval, so a batch of a 1 Hz `Lfo` read directly spans -1..1.
FUNCTION_NODE_ISLAND_READS_PER_REPORTED_BATCH = 120


class BatchesOfValuesReported:
    """Gathers the values a sink reads into batches, and reports the range of
    each batch with the sink's pid and when the batch started."""

    def __init__(self) -> None:
        self.values_in_this_batch: "list[float]" = []
        self.this_batch_started_at_monotonic_ns = time.monotonic_ns()

    def add(self, value: float) -> None:
        if not self.values_in_this_batch:
            self.this_batch_started_at_monotonic_ns = time.monotonic_ns()
        self.values_in_this_batch.append(float(value))
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


@node(execution="continuous", interval_ms=10)
def ReportsEachBatchOfFunctionNodeValuesItReadSink(
    *,
    brightness: InputPin[Annotated[float, StreamDelivery("newest")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads the value an island feeds `brightness`, and reports the range of
    each batch of reads with its own pid and when the batch started."""

    batches = BatchesOfValuesReported()

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        newest_brightness = brightness.read()
        if newest_brightness is not None:
            batches.add(newest_brightness)

    return each_frame


@node(execution="continuous", interval_ms=10)
def ReportsEachBatchOfItsLevel(
    *,
    level: Input[float] = Input(0.25),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads its `level` setting at each run, which a live set or a link changes,
    and reports the range of each batch."""

    batches = BatchesOfValuesReported()

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        batches.add(level.get())

    return each_frame


@node(execution="continuous", interval_ms=10)
def ReportsEachBatchOfPinValuesItReadSink(
    *,
    brightness: Input[float] = Input(0.25),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads `brightness`, which nothing is linked into and which declares 0.25,
    and reports the range of each batch of reads."""

    batches = BatchesOfValuesReported()

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        batches.add(brightness.get())

    return each_frame


@node
def SpinsForever(start: float = 1.0) -> float:  # noqa: N802
    """Never returns: a compiled call no thread can pre-empt, which only the watchdog ends."""
    climbing = start
    while climbing > 0.0:
        climbing = climbing + 1.0
    return climbing


@dataclasses.dataclass(frozen=True)
class CallsCountedSoFar:
    calls: float = 0.0


@node
def CountsItsOwnCalls(  # noqa: N802
    step: float = 1.0, *, previous: State[CallsCountedSoFar]
) -> Stateful[float, CallsCountedSoFar]:
    """The number of times it has been called, carried between calls in its state."""
    calls = previous.calls + step
    return calls, CallsCountedSoFar(calls)


@node
def LevelScaledByItsGain(level: float = 1.0, gain: float = 0.25) -> float:  # noqa: N802
    """Its level scaled by its `gain` input."""
    return level * gain


@node(execution="continuous", interval_ms=10)
class ReportsTheLevelItIsBuiltWith:
    """Reports `level`, a setting read at setup, each time it is built, with its own pid."""

    level: Annotated[float, SettingOnly()] = 0.1

    def __init__(self) -> None:
        report("BUILT_WITH_LEVEL", {"sink_pid": os.getpid(), "level": float(self.level)})

    def process(self, ctx) -> None:
        pass


@node
def HoldsItsLevelCountingItsCalls(  # noqa: N802
    step: float = 1.0, level: float = 0.25, *, previous: State[CallsCountedSoFar]
) -> Stateful[float, CallsCountedSoFar]:
    """Its `level` input, which a link can drive; stateful, so two readers move
    it into a helper of its own."""
    return level, CallsCountedSoFar(previous.calls + step)


#: The count at which a sink whose count started fresh takes its own helper down.
COUNT_AT_WHICH_A_FRESH_COUNT_SINK_DIES = 20.0


@node(execution="continuous", interval_ms=10)
def ReportsTheCountItReadAndDiesOnceAFreshCountReachesTwenty(
    *,
    count: Input[float | None] = Input(None),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reports the first count it reads with its own pid, and takes its own
    helper down once a count that started at one reaches twenty. A count it
    first reads above one was carried over, so it reads on without dying."""

    first_count_read: "float | None" = None

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal first_count_read
        if count.get() is None:
            return
        count_read = count.get()
        if first_count_read is None:
            first_count_read = count_read
            report("FIRST_COUNT_READ", {"pid": os.getpid(), "count": count_read})
        if first_count_read <= 1.0 and count_read >= COUNT_AT_WHICH_A_FRESH_COUNT_SINK_DIES:
            report("FRESH_COUNT_SINK_ABOUT_TO_DIE", {"pid": os.getpid(), "count": count_read})
            os._exit(1)

    return each_frame


#: The stream milliseconds between the steps of the stepped LFO stream.
STEPPED_LFO_STEP_MS = 100

#: Steps of the stepped LFO stream a sink reads before it reports them.
STEPPED_LFO_STEPS_REPORTED = 12


@node(execution="continuous", interval_ms=STEPPED_LFO_STEP_MS)
def ReportsTheLfoValueOfEachStepSink(
    *,
    wave: Input[float | None] = Input(None),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads the value an `Lfo` island evaluates to at each step, and reports
    the first `STEPPED_LFO_STEPS_REPORTED` with their step indices."""

    value_read_at_each_step: "dict[int, float]" = {}

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        step_index = ctx.stream_clock.step_index
        if step_index in value_read_at_each_step:
            return
        if len(value_read_at_each_step) >= STEPPED_LFO_STEPS_REPORTED:
            return
        if wave.get() is None:
            return
        value_read_at_each_step[step_index] = wave.get()
        if len(value_read_at_each_step) == STEPPED_LFO_STEPS_REPORTED:
            report(
                "STEPPED_LFO_VALUES_READ",
                {
                    "pid": os.getpid(),
                    "steps": [[step, value] for step, value in sorted(value_read_at_each_step.items())],
                },
            )

    return each_frame


#: How many counts a sink of a counter several readers read reads before it reports them.
COUNTS_READ_IN_ORDER_BEFORE_REPORTING = 20


@node(execution="continuous", interval_ms=10)
def ReportsTheCountsItReadInOrderSink(
    *,
    count: InputPin[Annotated[float, StreamDelivery("ordered")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads every count written on `count`, in order, and reports the first
    `COUNTS_READ_IN_ORDER_BEFORE_REPORTING` with its own pid."""

    counts_read: "list[float]" = []

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        while len(counts_read) < COUNTS_READ_IN_ORDER_BEFORE_REPORTING:
            newest_count = count.read()
            if newest_count is None:
                return
            counts_read.append(float(newest_count))
            if len(counts_read) == COUNTS_READ_IN_ORDER_BEFORE_REPORTING:
                report("COUNTS_READ_IN_ORDER", {"pid": os.getpid(), "counts": counts_read})

    return each_frame


#: How many records of a function node run as Python a sink reads before it reports them.
RECORDS_READS_BEFORE_REPORTING = 10


@dataclasses.dataclass(frozen=True)
class LevelLabel:
    """A level named, and spelled to two places."""

    label: str
    spelled: str


@node
def LabelsTheLevel(level: float = 0.75) -> LevelLabel:  # noqa: N802
    """Names the level it is handed: a body formatting a string, which runs as Python."""
    return LevelLabel(label="high" if level > 0.5 else "low", spelled=f"{level:.2f}")


@node(execution="continuous", interval_ms=10)
def ReportsTheRecordsItReadSink(
    *,
    label: Input[LevelLabel | None] = Input(None),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads the bundle a function node island feeds `label`, and reports its
    first bundles, as records, with its own pid."""

    records_read: "list[dict]" = []
    reported_its_start = False

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal reported_its_start
        if not reported_its_start:
            reported_its_start = True
            report("RECORDS_SINK_STARTED", {"pid": os.getpid()})
        if len(records_read) >= RECORDS_READS_BEFORE_REPORTING:
            return
        if label.get() is None:
            return
        records_read.append(dataclasses.asdict(label.get()))
        if len(records_read) == RECORDS_READS_BEFORE_REPORTING:
            report("RECORDS_READ", {"sink_pid": os.getpid(), "records": records_read})

    return each_frame


@node
def CrashesItsHelper(start: float = 1.0) -> float:  # noqa: N802
    """Reads address zero, which no compiled body can do, so it runs as Python and takes its helper down with SIGSEGV."""
    ctypes.string_at(0)
    return start


@node
def GateThatCrashesItsHelperOnCue[PinValue](value: PinValue, crash: bool = False) -> PinValue:  # noqa: N802
    """`value` handed on unread, as an open `Gate` hands it; while `crash` is set, it reads address
    zero and takes its helper down with SIGSEGV, which no compiled body can do, so it runs as Python."""
    if crash:
        ctypes.string_at(0)
    return value


#: How many levels a native converter casts of a pooled island's value a sink reads before it
#: reports them.
LEVELS_READ_BEFORE_REPORTING = 10


@node(execution="continuous", interval_ms=10)
def ReportsTheLevelsItReadSink(
    *,
    levels: InputPin[Annotated[int, StreamDelivery("newest")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads the integer a native converter casts of a pooled island's value,
    and reports its first levels with its own pid."""

    levels_read: "list[int]" = []

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        if len(levels_read) >= LEVELS_READ_BEFORE_REPORTING:
            return
        level = levels.read()
        if level is None:
            return
        levels_read.append(level)
        if len(levels_read) == LEVELS_READ_BEFORE_REPORTING:
            report("LEVELS_READ", {"sink_pid": os.getpid(), "levels": levels_read})

    return each_frame


@dataclasses.dataclass(frozen=True)
class Count:
    """A count, from 1."""

    count: int


@node(execution="continuous", interval_ms=10)
def CountsEachTickRecordsSource(
    *,
    counts_to_downstream: OutputPin[Count],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes a `Count` every tick, counting from 1."""

    _count = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal _count
        _count += 1
        counts_to_downstream.write(Count(count=_count))

    return each_frame


@node(execution="continuous", interval_ms=10)
def CountsEachTickAsALevelSource(
    *,
    level_to_downstream: OutputPin[float],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes the count every tick as a level, counting from 1."""

    count = 0

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal count
        count += 1
        level_to_downstream.write(float(count))

    return each_frame


#: How many reads a gated sink makes into each batch it reports.
GATED_READS_PER_REPORTED_BATCH = 50


@node(execution="continuous", interval_ms=10)
def ReportsEachBatchOfGatedCountsItReadSink(
    *,
    gated_counts: InputPin[Annotated[Count, StreamDelivery("newest")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads the counts a `Gate` hands on, and reports each batch of reads with
    its own pid and when the batch started: the counts it read, and how many
    reads found nothing."""

    counts_in_this_batch: "list[int]" = []
    reads_in_this_batch = 0
    this_batch_started_at_monotonic_ns = time.monotonic_ns()

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal counts_in_this_batch, reads_in_this_batch, this_batch_started_at_monotonic_ns
        if reads_in_this_batch == 0:
            this_batch_started_at_monotonic_ns = time.monotonic_ns()
        count = gated_counts.read()
        reads_in_this_batch += 1
        if count is not None:
            counts_in_this_batch.append(count.count)
        if reads_in_this_batch == GATED_READS_PER_REPORTED_BATCH:
            report(
                "GATED_COUNT_BATCH_READ",
                {
                    "sink_pid": os.getpid(),
                    "started_at_monotonic_ns": this_batch_started_at_monotonic_ns,
                    "counts": counts_in_this_batch,
                },
            )
            counts_in_this_batch = []
            reads_in_this_batch = 0

    return each_frame


@node(execution="manual")
def LendsTheLabelToItsNativeHalf(
    *,
    label: InputPin[Annotated[LevelLabel, StreamDelivery("newest")]],
    ctx: RuntimeContextFullAccess,
) -> None:
    """Lends `label` to native code at setup, as a module's native half does,
    and reports whether the lend was taken."""

    try:
        ctx.open_module_host_table(input_ports=["label"])
    except ValueError as refused:
        report("LABEL_LEND", {"pid": os.getpid(), "refused": str(refused)})
        return
    report("LABEL_LEND", {"pid": os.getpid(), "refused": None})


#: How long each call of `SleepsPastThePoolsFrameInterval` sleeps, past the pool's 10 ms interval.
SLEEP_OF_EACH_CALL_PAST_THE_POOLS_FRAME_INTERVAL_SECONDS = 0.02


def _sleep_past_the_pools_frame_interval() -> None:
    time.sleep(SLEEP_OF_EACH_CALL_PAST_THE_POOLS_FRAME_INTERVAL_SECONDS)


@node
def SleepsPastThePoolsFrameInterval(level: float = 0.5) -> float:  # noqa: N802
    """Gives `level` after a sleep longer than the pool's frame interval: a body calling a plain Python function, which runs as Python.

    The sleep sits in a helper because a function node's body may not reach the
    clock itself; the side-effect check reads only the body.
    """
    _sleep_past_the_pools_frame_interval()
    return level


#: How many calls `HangsAfterItsFirstCalls` returns from in each process before one never returns.
CALLS_RETURNED_BEFORE_A_CALL_HANGS = 100

_calls_of_hangs_after_its_first_calls_in_this_process = 0


def _count_this_call_and_hang_once_this_process_has_returned_enough() -> None:
    global _calls_of_hangs_after_its_first_calls_in_this_process
    _calls_of_hangs_after_its_first_calls_in_this_process += 1
    while _calls_of_hangs_after_its_first_calls_in_this_process > CALLS_RETURNED_BEFORE_A_CALL_HANGS:
        time.sleep(1.0)


@node
def HangsAfterItsFirstCalls(level: float = 0.25) -> float:  # noqa: N802
    """Gives `level` for its first calls in a process, then never returns: a body calling a plain Python function, which runs as Python.

    The per-process count sits in a helper because a function node's body may
    not write module state itself; the side-effect check reads only the body.
    """
    _count_this_call_and_hang_once_this_process_has_returned_enough()
    return level


#: How many counts a sink reads between reports.
COUNTS_READ_BETWEEN_REPORTS = 25


@node(execution="continuous", interval_ms=10)
def ReportsTheCountsItReadSink(
    *,
    counts: InputPin[Annotated[int, StreamDelivery("newest")]],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads the integer a native converter casts of a pooled counter, and
    reports each `COUNTS_READ_BETWEEN_REPORTS` counts it read."""

    counts_since_the_last_report: "list[int]" = []

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal counts_since_the_last_report
        count = counts.read()
        if count is None:
            return
        counts_since_the_last_report.append(count)
        if len(counts_since_the_last_report) == COUNTS_READ_BETWEEN_REPORTS:
            report("COUNTS_READ", {"pid": os.getpid(), "counts": counts_since_the_last_report})
            counts_since_the_last_report = []

    return each_frame


#: The width `WritesOneWidthSource` writes at each tick.
WIDTH_WRITTEN = 7

#: The width `ReportsEachBatchOfTheWidthItHolds` holds while no link drives it.
WIDTH_HELD_UNLINKED = 4


@node(execution="continuous", interval_ms=10)
def WritesOneWidthSource(
    *,
    width: OutputPin[int],
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Writes `WIDTH_WRITTEN` every tick."""

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        width.write(WIDTH_WRITTEN)

    return each_frame


@node(execution="continuous", interval_ms=10)
def ReportsEachBatchOfTheWidthItHolds(
    *,
    width: Input[int] = Input(WIDTH_HELD_UNLINKED),
) -> Callable[[RuntimeContextLimitedAccess], None]:
    """Reads its `width` setting at each run, as `SolidColor` reads its size, and reports the
    range of each batch with its own pid and when the batch started."""

    widths_in_this_batch: "list[int]" = []
    this_batch_started_at_monotonic_ns = time.monotonic_ns()

    def each_frame(ctx: RuntimeContextLimitedAccess) -> None:
        nonlocal this_batch_started_at_monotonic_ns, widths_in_this_batch
        if not widths_in_this_batch:
            this_batch_started_at_monotonic_ns = time.monotonic_ns()
        widths_in_this_batch.append(width.get())
        if len(widths_in_this_batch) == FUNCTION_NODE_ISLAND_READS_PER_REPORTED_BATCH:
            report(
                "FUNCTION_NODE_VALUE_BATCH_READ",
                {
                    "sink_pid": os.getpid(),
                    "started_at_monotonic_ns": this_batch_started_at_monotonic_ns,
                    "lowest": min(widths_in_this_batch),
                    "highest": max(widths_in_this_batch),
                },
            )
            widths_in_this_batch = []

    return each_frame
