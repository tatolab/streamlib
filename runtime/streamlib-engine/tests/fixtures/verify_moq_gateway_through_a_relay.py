#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The MoQ gateway's live proof through a real draft-16 relay, with a handoff
directory and per-track content keys, read back by an independent subscriber.

1. One runtime — a stamped-bag source, and a 1080p test pattern into
   `H264Encoder` — starts with `STREAMLIB_MOQ_GATEWAY_HANDOFF_DIR` holding
   only `relay.json`, and must announce a namespace (`GET /api/graph`).
2. Before any key exists, a subscriber to the bag track must see nothing.
3. `<runtime>.json` then carries an epoch-1 and an epoch-3 key per track; the
   subscriber, holding only the epoch-3 key, must read every one of `--count`
   objects on each track as a `SLE1` envelope sealed under epoch 3 that opens
   and parses (attachment plus msgpack bag, or a frame).
4. While a subscription is live, `/api/graph` must show the track
   `subscribed`, `servable`, and of the right kind, and the gateway `serving`.
5. No credential may appear in the runtime's log.

    set -a; . ./.env; set +a
    .venv/bin/python runtime/streamlib-engine/tests/fixtures/verify_moq_gateway_through_a_relay.py \\
        [--relay remote|local] [output_dir]

Exit status is the verdict: 0 pass, 1 fail, 77 cannot run (no credential, no
wheel, no relay, a runtime that could not start). The report is on stdout and
in the output directory, scrubbed of every credential.
"""

import argparse
import base64
import json
import os
import secrets
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

from moq_gateway_fixture_support import (
    EXIT_CANNOT_RUN,
    EXIT_FAIL,
    EXIT_PASS,
    FIXTURES,
    LOCAL_RELAY_URL,
    build_the_test_subscriber,
    cannot_run,
    carries_a_credential,
    free_loopback_ports,
    scrubbed,
    start_the_local_relay,
    stop_a_process_group,
    the_engine_wheel_is_importable,
    the_remote_relay_url_or_cannot_run,
)

EXPECTED_KIND_BY_TRACK = {
    "BenchSource/stamps": "bags",
    "BenchEncoder/encoded_video": "video",
}
HOW_LONG_THE_GATEWAY_HAS_TO_ANNOUNCE_SECONDS = 60


def base64url(key: bytes) -> str:
    return base64.urlsafe_b64encode(key).decode().rstrip("=")


def the_graph(port: int) -> dict:
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/graph", timeout=5) as response:
        return json.loads(response.read())


def run_the_test_subscriber(
    subscriber: Path, relay_url: str, namespace: str, track: str, key: "bytes | None",
    count: int, timeout_seconds: int, accept_any_certificate: bool,
) -> "tuple[int, list[dict], dict, str]":
    environment = dict(os.environ)
    environment["MOQ_TEST_RELAY_URL"] = relay_url
    environment.pop("MOQ_TEST_CONTENT_KEY_BASE64URL", None)
    if key is not None:
        environment["MOQ_TEST_CONTENT_KEY_BASE64URL"] = base64url(key)
    completed = subprocess.run(
        [str(subscriber), "--namespace", namespace, "--track", track, "--count", str(count),
         "--timeout-seconds", str(timeout_seconds),
         *(["--accept-any-certificate"] if accept_any_certificate else [])],
        env=environment, capture_output=True, text=True,
    )
    lines = [json.loads(line) for line in completed.stdout.splitlines() if line.startswith("{")]
    objects = [line for line in lines if "summary" not in line]
    summary = next((line for line in lines if "summary" in line), {})
    return completed.returncode, objects, summary, scrubbed(completed.stderr)[-2000:]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--relay", choices=["remote", "local"], default="remote")
    parser.add_argument("--count", type=int, default=90)
    parser.add_argument("output_dir", nargs="?")
    arguments = parser.parse_args()
    output = Path(arguments.output_dir or tempfile.mkdtemp(prefix="streamlib-moq-gateway-"))
    output.mkdir(parents=True, exist_ok=True)

    the_engine_wheel_is_importable()
    if arguments.relay == "remote":
        relay_url = the_remote_relay_url_or_cannot_run()
    else:
        relay_url = LOCAL_RELAY_URL
    accept_any_certificate = arguments.relay == "local"
    subscriber = build_the_test_subscriber(output / "test-subscriber-build.log")
    local_relay = start_the_local_relay(output) if arguments.relay == "local" else None

    (control_plane_port,) = free_loopback_ports(1)
    runtime_name = f"moq-gateway-verify-{secrets.token_hex(3)}"
    handoff = Path(tempfile.mkdtemp(prefix="streamlib-moq-gateway-handoff-"))
    (handoff / "relay.json").write_text(json.dumps(
        {"relayPublishUrl": relay_url, "namespacePrefix": "streamlib-fixture/moq-gateway"}
    ))
    environment = dict(os.environ)
    for inherited in ("STREAMLIB_MESH_TRANSPORT", "STREAMLIB_MESH_MOQ_RELAY_URL",
                      "STREAMLIB_MESH_MOQ_NAMESPACE_PREFIX"):
        environment.pop(inherited, None)
    environment.update({
        "STREAMLIB_MOQ_GATEWAY_HANDOFF_DIR": str(handoff),
        "STREAMLIB_MESH_NAME": f"moqgw{secrets.token_hex(3)}",
        "STREAMLIB_MESH_MULTICAST_DISCOVERY": "0",
        "PYTHONPATH": str(FIXTURES) + os.pathsep + environment.get("PYTHONPATH", ""),
        "MOQ_BENCH_RATES": "50",
        "MOQ_BENCH_STEP_SECONDS": "600",
        "MOQ_BENCH_START_DELAY_SECONDS": "0",
    })
    if accept_any_certificate:
        environment["STREAMLIB_MESH_MOQ_DANGER_ACCEPT_ANY_CERTIFICATE"] = "1"

    node_log_path = output / "node.log"
    node_log = open(node_log_path, "w")
    node = subprocess.Popen(
        [sys.executable, str(FIXTURES / "moq_gateway_node.py"), "--role", "source",
         "--workload", "smoke", "--runtime-name", runtime_name,
         "--control-plane-port", str(control_plane_port)],
        env=environment, cwd=output, stdout=node_log, stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    report: dict = {"relay": arguments.relay, "runtime_name": runtime_name, "tracks": {},
                    "failures": []}
    failures: "list[str]" = report["failures"]
    exit_status_it_stopped_with = None
    try:
        namespace = ""
        deadline = time.monotonic() + HOW_LONG_THE_GATEWAY_HAS_TO_ANNOUNCE_SECONDS
        while time.monotonic() < deadline and not namespace:
            time.sleep(1)
            if node.poll() is not None:
                node_log.flush()
                if "Traceback (most recent call last)" in node_log_path.read_text(errors="replace"):
                    failures.append("the runtime's script raised before its gateway announced")
                    return EXIT_FAIL
                cannot_run(f"the runtime could not start; see {node_log_path}")
            try:
                namespace = the_graph(control_plane_port).get("moq_gateway", {}).get("namespace", "")
            except OSError:
                continue
        if not namespace:
            failures.append("the gateway never announced a namespace")
            return EXIT_FAIL
        report["namespace"] = namespace

        # Nothing may be published before the handoff carries a key.
        status, objects, _, _ = run_the_test_subscriber(
            subscriber, relay_url, namespace, "BenchSource/stamps", None, 1, 8,
            accept_any_certificate)
        report["objects_before_any_key"] = len(objects)
        if objects:
            failures.append(f"{len(objects)} object(s) were published before any key existed")

        keys = {track: secrets.token_bytes(16) for track in EXPECTED_KIND_BY_TRACK}
        (handoff / f"{runtime_name}.json").write_text(json.dumps({
            "paused": False,
            "contentKeys": {
                f"{namespace}/{track}": [
                    {"epoch": 1, "keyBase64Url": base64url(bytes(16))},
                    {"epoch": 3, "keyBase64Url": base64url(key)},
                ]
                for track, key in keys.items()
            },
        }))
        time.sleep(3)

        for track, expected_kind in EXPECTED_KIND_BY_TRACK.items():
            graph_while_subscribed: dict = {}

            def sample_the_graph_midway() -> None:
                time.sleep(6)
                try:
                    graph_while_subscribed.update(the_graph(control_plane_port))
                except OSError as failure:
                    graph_while_subscribed["error"] = str(failure)

            sampler = threading.Thread(target=sample_the_graph_midway)
            sampler.start()
            status, objects, summary, stderr = run_the_test_subscriber(
                subscriber, relay_url, namespace, track, keys[track], arguments.count, 40,
                accept_any_certificate)
            sampler.join()
            gateway = graph_while_subscribed.get("moq_gateway", {})
            listed = next((entry for entry in gateway.get("tracks", [])
                           if entry["relay_track"] == track), None)
            report["tracks"][track] = {
                "subscriber_exit": status, "subscriber_stderr": stderr, "summary": summary,
                "key_epochs_seen": sorted({o.get("key_epoch") for o in objects}, key=str),
                "first_objects": objects[:3], "graph_track_while_subscribed": listed,
                "graph_gateway_state_while_subscribed": gateway.get("state"),
            }
            if status != 0:
                failures.append(f"{track}: the subscriber exited {status}")
            if len(objects) != arguments.count:
                failures.append(f"{track}: {len(objects)} of {arguments.count} objects arrived")
            if not all(o.get("sealed") and o.get("decrypted") and o.get("key_epoch") == 3
                       for o in objects):
                failures.append(f"{track}: an object was not sealed under epoch 3 and opened")
            if summary.get("parsed") != len(objects):
                failures.append(f"{track}: {summary.get('parsed')} of {len(objects)} parsed")
            if gateway.get("state") != "serving":
                failures.append(f"{track}: the gateway read {gateway.get('state')!r}, not serving")
            if not listed or not (listed["subscribed"] and listed["servable"]
                                  and listed["kind"] == expected_kind):
                failures.append(f"{track}: graph listed it as {listed}, not a subscribed, "
                                f"servable {expected_kind} track")
    except SystemExit as stopping:
        exit_status_it_stopped_with = stopping.code
        raise
    finally:
        stop_a_process_group(node)
        node_log.close()
        raw_node_log = node_log_path.read_text(errors="replace")
        if carries_a_credential(raw_node_log):
            failures.append("the runtime's log carried a credential")
        node_log_path.write_text(scrubbed(raw_node_log))
        for leftover in handoff.iterdir():
            leftover.unlink()
        handoff.rmdir()
        if local_relay is not None:
            stop_a_process_group(local_relay)
        if exit_status_it_stopped_with == EXIT_CANNOT_RUN:
            report["verdict"] = "CANNOT RUN"
        else:
            report["verdict"] = "FAIL" if failures else "PASS"
        rendered = scrubbed(json.dumps(report, indent=1))
        (output / "report.json").write_text(rendered)
        print(rendered)
        print(f"{report['verdict']}: output in {output}", file=sys.stderr)
    return EXIT_FAIL if failures else EXIT_PASS


if __name__ == "__main__":
    sys.exit(main())
