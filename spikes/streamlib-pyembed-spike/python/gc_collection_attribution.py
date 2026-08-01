# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Timestamps every CPython garbage collection so a latency tail spike in the
Rust-side per-frame JSONL can be attributed to a GC pause rather than to PyO3."""

import gc
import json
import os
import time

# The Rust side stamps raw CLOCK_MONOTONIC via libc::clock_gettime
# (src/monotonic_clock.rs). `time.monotonic_ns()` reports
# implementation='clock_gettime(CLOCK_MONOTONIC)' on Linux and would agree here,
# but it is mach_absolute_time on macOS and QueryPerformanceCounter on Windows --
# a different epoch, silently. Naming the clock explicitly is what makes GC
# stamps and frame stamps joinable by construction rather than by platform luck.
MONOTONIC_CLOCK_IDENTIFIER = time.CLOCK_MONOTONIC

GARBAGE_COLLECTION_MODE_DEFAULT = "default"
GARBAGE_COLLECTION_MODE_TUNED = "tuned"
SUPPORTED_GARBAGE_COLLECTION_MODES = (
    GARBAGE_COLLECTION_MODE_DEFAULT,
    GARBAGE_COLLECTION_MODE_TUNED,
)

COLLECTION_PHASE_START = "start"
COLLECTION_PHASE_STOP = "stop"

# Generation-0 fires once per (allocations - deallocations) exceeding its
# threshold; CPython's stock 700 means a numpy stage allocating a handful of
# temporaries per frame triggers a sweep every few frames. 50_000 pushes it well
# past a whole measurement cell's churn. Generations 1 and 2 count collections of
# the generation below, so 100/100 turns a stock gen-2 sweep every ~70_000
# allocations into one every ~350_000_000.
TUNED_GENERATION_ZERO_ALLOCATION_SURPLUS_THRESHOLD = 50_000
TUNED_GENERATION_ONE_COLLECTION_COUNT_THRESHOLD = 100
TUNED_GENERATION_TWO_COLLECTION_COUNT_THRESHOLD = 100


def read_raw_monotonic_clock_nanoseconds():
    """Read CLOCK_MONOTONIC in nanoseconds, the one clock every spike arm shares."""
    return time.clock_gettime_ns(MONOTONIC_CLOCK_IDENTIFIER)


class GarbageCollectionPauseAttributionRecorder:
    """Records phase-stamped CPython collection events and the GC configuration
    they ran under, for joining against the Rust-side per-frame latency JSONL."""

    def __init__(self):
        self._collection_phase_events = []
        self._pending_collection_start_stamp_by_generation = {}
        self._collection_phase_callback = None
        self._applied_garbage_collection_mode = GARBAGE_COLLECTION_MODE_DEFAULT
        self._garbage_collection_thresholds_at_install = gc.get_threshold()
        self._frozen_object_count_after_tuning = None

    def install_collection_phase_callback(self):
        """Start recording collection events. Idempotent."""
        if self._collection_phase_callback is not None:
            return

        collection_phase_events = self._collection_phase_events

        def record_collection_phase_event(collection_phase, collection_info):
            # Stamp first: everything below is recorded latency that did not
            # happen inside the collector.
            collection_phase_stamp_nanoseconds = time.clock_gettime_ns(
                MONOTONIC_CLOCK_IDENTIFIER
            )
            # Appending a flat tuple rather than building a dict keeps this
            # callback from allocating the very containers it is measuring the
            # sweep of. Records are expanded to dicts at export time.
            collection_phase_events.append(
                (
                    collection_phase,
                    collection_info.get("generation"),
                    collection_info.get("collected"),
                    collection_info.get("uncollectable"),
                    collection_phase_stamp_nanoseconds,
                )
            )

        self._collection_phase_callback = record_collection_phase_event
        gc.callbacks.append(record_collection_phase_event)

    def uninstall_collection_phase_callback(self):
        """Stop recording collection events. Idempotent."""
        if self._collection_phase_callback is None:
            return
        if self._collection_phase_callback in gc.callbacks:
            gc.callbacks.remove(self._collection_phase_callback)
        self._collection_phase_callback = None

    def apply_garbage_collection_mode(self, garbage_collection_mode):
        """Apply `default` or `tuned` GC settings, returning the resolved
        configuration for the cell spec."""
        if garbage_collection_mode not in SUPPORTED_GARBAGE_COLLECTION_MODES:
            raise ValueError(
                "unsupported garbage collection mode "
                f"{garbage_collection_mode!r}; expected one of "
                f"{SUPPORTED_GARBAGE_COLLECTION_MODES}"
            )

        self._applied_garbage_collection_mode = garbage_collection_mode
        if garbage_collection_mode == GARBAGE_COLLECTION_MODE_TUNED:
            # A full sweep first so everything reachable at this point is
            # settled, then gc.freeze() moves every currently-tracked object into
            # the permanent generation, which no collection ever traverses again.
            # Interpreter startup, module import and numpy's own type objects
            # account for thousands of long-lived containers that a stock gen-2
            # sweep re-walks every time -- the exact work that shows up as a
            # multi-millisecond tail spike in a 16.6ms frame budget.
            gc.collect()
            gc.freeze()
            self._frozen_object_count_after_tuning = gc.get_freeze_count()
            gc.set_threshold(
                TUNED_GENERATION_ZERO_ALLOCATION_SURPLUS_THRESHOLD,
                TUNED_GENERATION_ONE_COLLECTION_COUNT_THRESHOLD,
                TUNED_GENERATION_TWO_COLLECTION_COUNT_THRESHOLD,
            )
        self._garbage_collection_thresholds_at_install = gc.get_threshold()
        return self.report_garbage_collection_configuration()

    def report_garbage_collection_configuration(self):
        """The GC configuration actually in force, for recording into the cell spec."""
        return {
            "applied_garbage_collection_mode": self._applied_garbage_collection_mode,
            "garbage_collector_enabled": gc.isenabled(),
            "generation_thresholds": list(gc.get_threshold()),
            "frozen_permanent_generation_object_count": gc.get_freeze_count(),
            "frozen_permanent_generation_object_count_immediately_after_tuning": (
                self._frozen_object_count_after_tuning
            ),
            "collection_phase_callback_installed": (
                self._collection_phase_callback is not None
            ),
            "monotonic_clock_identifier": "CLOCK_MONOTONIC",
            "monotonic_clock_implementation": (
                time.get_clock_info("monotonic").implementation
            ),
        }

    def recorded_collection_phase_event_count(self):
        """Number of phase events recorded so far (two per completed collection)."""
        return len(self._collection_phase_events)

    def export_collection_records_as_jsonl(self, output_path):
        """Write one JSON object per recorded phase event to `output_path`,
        returning the number of lines written."""
        self._pending_collection_start_stamp_by_generation = {}
        output_directory = os.path.dirname(os.path.abspath(output_path))
        if output_directory:
            os.makedirs(output_directory, exist_ok=True)

        written_line_count = 0
        with open(output_path, "w", encoding="utf-8") as jsonl_output_file:
            for collection_phase_event in self._collection_phase_events:
                jsonl_output_file.write(
                    json.dumps(self._expand_collection_phase_event(collection_phase_event))
                )
                jsonl_output_file.write("\n")
                written_line_count += 1
        return written_line_count

    def _expand_collection_phase_event(self, collection_phase_event):
        (
            collection_phase,
            generation,
            collected_object_count,
            uncollectable_object_count,
            collection_phase_stamp_nanoseconds,
        ) = collection_phase_event

        collection_record = {
            "collection_phase": collection_phase,
            "generation": generation,
            # CPython zero-fills both counters on the `start` phase and only
            # populates them on `stop` (verified on CPython 3.12.3); the start
            # values are recorded as-reported and must not be read as results.
            "collected_object_count": collected_object_count,
            "uncollectable_object_count": uncollectable_object_count,
            "collection_phase_monotonic_nanoseconds": collection_phase_stamp_nanoseconds,
            "collection_start_monotonic_nanoseconds": None,
            "collection_stop_monotonic_nanoseconds": None,
            "collection_duration_nanoseconds": None,
        }

        if collection_phase == COLLECTION_PHASE_START:
            self._pending_collection_start_stamp_by_generation[generation] = (
                collection_phase_stamp_nanoseconds
            )
            collection_record["collection_start_monotonic_nanoseconds"] = (
                collection_phase_stamp_nanoseconds
            )
            return collection_record

        collection_record["collection_stop_monotonic_nanoseconds"] = (
            collection_phase_stamp_nanoseconds
        )
        collection_start_stamp_nanoseconds = (
            self._pending_collection_start_stamp_by_generation.pop(generation, None)
        )
        if collection_start_stamp_nanoseconds is not None:
            collection_record["collection_start_monotonic_nanoseconds"] = (
                collection_start_stamp_nanoseconds
            )
            collection_record["collection_duration_nanoseconds"] = (
                collection_phase_stamp_nanoseconds - collection_start_stamp_nanoseconds
            )
        return collection_record

    def __enter__(self):
        self.install_collection_phase_callback()
        return self

    def __exit__(self, exception_type, exception_value, exception_traceback):
        self.uninstall_collection_phase_callback()
        return False
