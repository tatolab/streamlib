#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Measure a remote link carried three ways: over Zenoh, over MoQ through a
local draft-16 relay, and over MoQ through a remote relay. Two runtimes as
separate processes on this host, one pulling the other's port.

Same host, one monotonic clock: latency is sink-now minus source-stamp. This
is NOT a cross-host measurement.

    set -a; . ./.env; set +a
    .venv/bin/python runtime/streamlib-engine/tests/fixtures/measure_moq_gateway_against_zenoh.py \\
        [--transports zenoh,moq-local,moq-remote] [--workloads bags,video] [output_dir]

A measurement, not a gate: it exits 0 with a table whichever way the numbers
land, 1 if an arm produced no results, and 77 if a requested arm cannot run
here (no relay credential for `moq-remote`, no wheel, no local relay). The
remote relay's credential is read as `moq_gateway_fixture_support` describes
and scrubbed from every line printed or saved.
"""

import argparse
import json
import os
import secrets
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from moq_gateway_fixture_support import (
    EXIT_FAIL,
    EXIT_PASS,
    FIXTURES,
    LOCAL_RELAY_URL,
    free_loopback_ports,
    scrubbed,
    start_the_local_relay,
    stop_a_process_group,
    the_engine_wheel_is_importable,
    the_remote_relay_url_or_cannot_run,
)

TRANSPORTS = ("zenoh", "moq-local", "moq-remote")


def the_environment_for(transport: str, run_id: str, results_path: Path,
                        remote_relay_url: "str | None") -> dict:
    environment = dict(os.environ)
    for inherited in (
        "STREAMLIB_MESH_TRANSPORT",
        "STREAMLIB_MESH_MOQ_RELAY_URL",
        "STREAMLIB_MESH_MOQ_NAMESPACE_PREFIX",
        "STREAMLIB_MESH_MOQ_DANGER_ACCEPT_ANY_CERTIFICATE",
        "STREAMLIB_MOQ_GATEWAY_HANDOFF_DIR",
    ):
        environment.pop(inherited, None)
    environment.update({
        "STREAMLIB_MESH_NAME": f"moqbench{run_id}",
        "STREAMLIB_MESH_MULTICAST_DISCOVERY": "0",
        "MOQ_BENCH_RESULTS_PATH": str(results_path),
        "PYTHONPATH": str(FIXTURES) + os.pathsep + environment.get("PYTHONPATH", ""),
    })
    if transport == "moq-local":
        environment.update({
            "STREAMLIB_MESH_TRANSPORT": "moq",
            "STREAMLIB_MESH_MOQ_RELAY_URL": LOCAL_RELAY_URL,
            "STREAMLIB_MESH_MOQ_NAMESPACE_PREFIX": f"streamlib-bench/{run_id}",
            "STREAMLIB_MESH_MOQ_DANGER_ACCEPT_ANY_CERTIFICATE": "1",
        })
    elif transport == "moq-remote":
        environment.update({
            "STREAMLIB_MESH_TRANSPORT": "moq",
            "STREAMLIB_MESH_MOQ_RELAY_URL": remote_relay_url or "",
            "STREAMLIB_MESH_MOQ_NAMESPACE_PREFIX": f"streamlib-bench/{run_id}",
        })
    return environment


def run_one(transport: str, workload: str, arguments, log_directory: Path,
            remote_relay_url: "str | None") -> dict:
    run_id = secrets.token_hex(3)
    results_path = log_directory / f"{transport}-{workload}.json"
    results_path.unlink(missing_ok=True)
    environment = the_environment_for(transport, run_id, results_path, remote_relay_url)
    source_name = f"moq-bench-source-{run_id}"
    zenoh_port, sink_zenoh_port, source_control_port, sink_control_port = free_loopback_ports(4)
    source_environment = dict(environment)
    source_environment["STREAMLIB_MESH_LISTEN_ENDPOINTS"] = f"udp/127.0.0.1:{zenoh_port}?rel=1"
    sink_environment = dict(environment)
    sink_environment["STREAMLIB_MESH_LISTEN_ENDPOINTS"] = f"udp/127.0.0.1:{sink_zenoh_port}?rel=1"
    sink_environment["STREAMLIB_MESH_PEER_ENDPOINTS"] = f"udp/127.0.0.1:{zenoh_port}?rel=1"

    node = str(FIXTURES / "moq_gateway_node.py")
    source_log_path = log_directory / f"{transport}-{workload}-source.log"
    sink_log_path = log_directory / f"{transport}-{workload}-sink.log"
    source_log = open(source_log_path, "w")
    sink_log = open(sink_log_path, "w")
    source = subprocess.Popen(
        [sys.executable, node, "--role", "source", "--workload", workload,
         "--runtime-name", source_name, "--control-plane-port", str(source_control_port)],
        env=source_environment, cwd=FIXTURES, stdout=source_log, stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    time.sleep(2.0)
    sink = subprocess.Popen(
        [sys.executable, node, "--role", "sink", "--workload", workload,
         "--runtime-name", f"moq-bench-sink-{run_id}", "--source-runtime-name", source_name,
         "--control-plane-port", str(sink_control_port)],
        env=sink_environment, cwd=FIXTURES, stdout=sink_log, stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    if workload == "bags":
        rates = os.environ.get("MOQ_BENCH_RATES", "100,500,1000,2000,5000").split(",")
        duration = (
            float(os.environ.get("MOQ_BENCH_START_DELAY_SECONDS", "10"))
            + len(rates) * float(os.environ.get("MOQ_BENCH_STEP_SECONDS", "6"))
            + 4
        )
    else:
        duration = arguments.video_seconds
    print(f"[{transport}/{workload}] running for {duration:.0f}s (run {run_id})", flush=True)
    time.sleep(duration)
    stop_a_process_group(sink)
    stop_a_process_group(source)
    source_log.close()
    sink_log.close()
    for log_path in (source_log_path, sink_log_path):
        log_path.write_text(scrubbed(log_path.read_text(errors="replace")))
    if not results_path.exists():
        return {"error": "the sink wrote no results"}
    return json.loads(results_path.read_text())


def summarise(transport: str, workload: str, results: dict) -> "list[str]":
    if "error" in results:
        return [f"| {transport} | {workload} | ERROR: {results['error']} | | | | | |"]
    lines = []
    sustained = 0
    for step, record in sorted(results.items(), key=lambda item: int(item[0])):
        if int(step) < 0:
            continue
        lost = record["lost"]
        if workload == "bags" and lost == 0 and record["received"] > 0:
            sustained = max(sustained, record["rate"])
        lines.append(
            f"| {transport} | {workload} | {record['rate']} | {record['received']} | {lost} | "
            f"{record['p50_ms']:.2f} | {record['p99_ms']:.2f} | {record['max_ms']:.2f} |"
        )
    if workload == "bags":
        lines.append(f"| {transport} | {workload} | max lossless rate: {sustained}/s | | | | | |")
    return lines


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--transports", default=",".join(TRANSPORTS))
    parser.add_argument("--workloads", default="bags,video")
    parser.add_argument("--video-seconds", type=float, default=40)
    parser.add_argument("output_dir", nargs="?")
    arguments = parser.parse_args()
    log_directory = Path(arguments.output_dir or tempfile.mkdtemp(prefix="streamlib-moq-bench-"))
    log_directory = log_directory.resolve()
    log_directory.mkdir(parents=True, exist_ok=True)

    transports = arguments.transports.split(",")
    unknown = [transport for transport in transports if transport not in TRANSPORTS]
    if unknown:
        parser.error(f"unknown transport(s) {unknown}; choose from {TRANSPORTS}")
    the_engine_wheel_is_importable()
    remote_relay_url = the_remote_relay_url_or_cannot_run() if "moq-remote" in transports else None
    relay = start_the_local_relay(log_directory) if "moq-local" in transports else None
    table = [
        "| transport | workload | rate (/s, or fps for video) | received | lost | p50 ms | p99 ms | max ms |",
        "|---|---|---|---|---|---|---|---|",
    ]
    all_results = {}
    try:
        for workload in arguments.workloads.split(","):
            for transport in transports:
                results = run_one(transport, workload, arguments, log_directory, remote_relay_url)
                all_results[f"{transport}/{workload}"] = results
                table.extend(summarise(transport, workload, results))
                time.sleep(3)
    finally:
        if relay is not None:
            stop_a_process_group(relay)
    (log_directory / "summary.md").write_text("\n".join(table) + "\n")
    (log_directory / "all-results.json").write_text(scrubbed(json.dumps(all_results, indent=1)))
    print("\n".join(table))
    print(f"output in {log_directory}", file=sys.stderr)
    return EXIT_FAIL if any("error" in results for results in all_results.values()) else EXIT_PASS


if __name__ == "__main__":
    sys.exit(main())
