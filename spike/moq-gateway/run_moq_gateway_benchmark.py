#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Run the MoQ gateway spike benchmark: two runtimes as separate processes on
this host, a remote link between them over Zenoh, over MoQ through a local
relay, and over MoQ through Cloudflare's relay.

Same host, one monotonic clock: latency is sink-now minus source-stamp. This
is NOT a cross-host measurement.

    .venv/bin/python spike/moq-gateway/run_moq_gateway_benchmark.py \
        --transports zenoh,moq-local,moq-cloudflare --workloads bags,video

The Cloudflare arm reads CLOUDFLARE_MOQ_DRAFT_16_URL and
CLOUDFLARE_MOQ_PUB_SUB_TOKEN from the environment (source the repo's .env
first); the token is passed to the runtimes by environment only and scrubbed
from every line this script prints or saves.
"""

import argparse
import json
import os
import secrets
import signal
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
LOCAL_RELAY_URL = "https://localhost:4443/local"


def the_token() -> str:
    return os.environ.get("CLOUDFLARE_MOQ_PUB_SUB_TOKEN", "")


def scrubbed(text: str) -> str:
    token = the_token()
    return text.replace(token, "<token>") if token else text


def the_environment_for(transport: str, run_id: str, workload: str, results_path: Path) -> dict:
    environment = dict(os.environ)
    for inherited in (
        "STREAMLIB_MESH_TRANSPORT",
        "STREAMLIB_MESH_MOQ_RELAY_URL",
        "STREAMLIB_MESH_MOQ_NAMESPACE_PREFIX",
        "STREAMLIB_MESH_MOQ_DANGER_ACCEPT_ANY_CERTIFICATE",
        "STREAMLIB_MOQ_GATEWAY_HANDOFF_DIR",
    ):
        environment.pop(inherited, None)
    environment.update(
        {
            "STREAMLIB_MESH_NAME": "moqspikebench",
            "STREAMLIB_MESH_MULTICAST_DISCOVERY": "0",
            "MOQ_BENCH_RESULTS_PATH": str(results_path),
            "PYTHONPATH": str(HERE) + os.pathsep + environment.get("PYTHONPATH", ""),
        }
    )
    if transport == "moq-local":
        environment.update(
            {
                "STREAMLIB_MESH_TRANSPORT": "moq",
                "STREAMLIB_MESH_MOQ_RELAY_URL": LOCAL_RELAY_URL,
                "STREAMLIB_MESH_MOQ_NAMESPACE_PREFIX": f"bench/{run_id}",
                "STREAMLIB_MESH_MOQ_DANGER_ACCEPT_ANY_CERTIFICATE": "1",
            }
        )
    elif transport == "moq-cloudflare":
        host = os.environ["CLOUDFLARE_MOQ_DRAFT_16_URL"].strip().removeprefix("https://").strip("/")
        environment.update(
            {
                "STREAMLIB_MESH_TRANSPORT": "moq",
                "STREAMLIB_MESH_MOQ_RELAY_URL": f"https://{host}/{the_token()}",
                "STREAMLIB_MESH_MOQ_NAMESPACE_PREFIX": f"streamlib-spike-bench/{run_id}",
            }
        )
    return environment


def start_the_local_relay(log_directory: Path) -> subprocess.Popen:
    log = open(log_directory / "local-relay.log", "w")
    relay = subprocess.Popen(
        ["bash", str(HERE / "start_local_moq_relay.sh")],
        stdout=log,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    time.sleep(1.5)
    return relay


def stop(process: subprocess.Popen) -> None:
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait(timeout=5)


def run_one(transport: str, workload: str, arguments, log_directory: Path) -> dict:
    run_id = secrets.token_hex(3)
    results_path = log_directory / f"{transport}-{workload}.json"
    results_path.unlink(missing_ok=True)
    environment = the_environment_for(transport, run_id, workload, results_path)
    source_name = f"moq-bench-source-{run_id}"
    source_environment = dict(environment)
    source_environment["STREAMLIB_MESH_LISTEN_ENDPOINTS"] = f"udp/127.0.0.1:{arguments.zenoh_port}?rel=1"
    sink_environment = dict(environment)
    sink_environment["STREAMLIB_MESH_LISTEN_ENDPOINTS"] = f"udp/127.0.0.1:{arguments.zenoh_port + 1}?rel=1"
    sink_environment["STREAMLIB_MESH_PEER_ENDPOINTS"] = f"udp/127.0.0.1:{arguments.zenoh_port}?rel=1"

    python = sys.executable
    node = str(HERE / "moq_gateway_bench_node.py")
    source_log = open(log_directory / f"{transport}-{workload}-source.log", "w")
    sink_log = open(log_directory / f"{transport}-{workload}-sink.log", "w")
    source = subprocess.Popen(
        [python, node, "--role", "source", "--workload", workload, "--runtime-name", source_name,
         "--control-plane-port", str(arguments.control_plane_port)],
        env=source_environment, cwd=HERE, stdout=source_log, stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    time.sleep(2.0)
    sink = subprocess.Popen(
        [python, node, "--role", "sink", "--workload", workload,
         "--runtime-name", f"moq-bench-sink-{run_id}", "--source-runtime-name", source_name,
         "--control-plane-port", str(arguments.control_plane_port + 1)],
        env=sink_environment, cwd=HERE, stdout=sink_log, stderr=subprocess.STDOUT,
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
    stop(sink)
    stop(source)
    source_log.close()
    sink_log.close()
    for log_path in (log_directory / f"{transport}-{workload}-source.log",
                     log_directory / f"{transport}-{workload}-sink.log"):
        log_path.write_text(scrubbed(log_path.read_text(errors="replace")))
    if not results_path.exists():
        return {"error": "the sink wrote no results"}
    return json.loads(results_path.read_text())


def summarise(transport: str, workload: str, results: dict) -> "list[str]":
    if "error" in results:
        return [f"| {transport} | {workload} | ERROR: {results['error']} | | | | |"]
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


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--transports", default="zenoh,moq-local,moq-cloudflare")
    parser.add_argument("--workloads", default="bags,video")
    parser.add_argument("--video-seconds", type=float, default=40)
    parser.add_argument("--zenoh-port", type=int, default=17447)
    parser.add_argument("--control-plane-port", type=int, default=19400)
    parser.add_argument("--output", default=str(HERE / "results"))
    arguments = parser.parse_args()
    log_directory = Path(arguments.output).resolve()
    log_directory.mkdir(parents=True, exist_ok=True)

    transports = arguments.transports.split(",")
    relay = start_the_local_relay(log_directory) if "moq-local" in transports else None
    table = [
        "| transport | workload | rate (/s, or fps for video) | received | lost | p50 ms | p99 ms | max ms |",
        "|---|---|---|---|---|---|---|---|",
    ]
    all_results = {}
    try:
        for workload in arguments.workloads.split(","):
            for transport in transports:
                results = run_one(transport, workload, arguments, log_directory)
                all_results[f"{transport}/{workload}"] = results
                table.extend(summarise(transport, workload, results))
                time.sleep(3)
    finally:
        if relay is not None:
            stop(relay)
    (log_directory / "summary.md").write_text("\n".join(table) + "\n")
    (log_directory / "all-results.json").write_text(json.dumps(all_results, indent=1))
    print("\n".join(table))


if __name__ == "__main__":
    main()
