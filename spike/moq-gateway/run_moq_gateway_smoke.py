#!/usr/bin/env python3
# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""Live smoke for the MoQ gateway through a real relay, with a
handoff directory and per-track content keys.

1. Starts one runtime (a stamped-bag source, and a 1080p test pattern into
   H264Encoder) with STREAMLIB_MOQ_GATEWAY_HANDOFF_DIR pointing at a fresh
   directory holding only relay.json.
2. Reads the announced namespace off GET /api/graph, then writes
   <runtime>.json with a random 16-byte key for each served track.
3. Runs the independent test subscriber against each track with its key and
   checks every object is a SLE1 envelope that decrypts and parses.
4. While a subscription is live, checks /api/graph reports it subscribed.

    set -a; . ./.env; set +a
    .venv/bin/python spike/moq-gateway/run_moq_gateway_smoke.py [--relay cloudflare|local]

The relay token is passed by environment only and scrubbed from what is saved.
"""

import argparse
import base64
import json
import os
import secrets
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
SUBSCRIBER = HERE / "moq-gateway-test-subscriber" / "target" / "release" / "moq-gateway-test-subscriber"
TRACKS = ["BenchSource/stamps", "BenchEncoder/encoded_video"]


def scrub(text: str) -> str:
    token = os.environ.get("CLOUDFLARE_MOQ_PUB_SUB_TOKEN", "")
    return text.replace(token, "<token>") if token else text


def the_graph(port: int) -> dict:
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/api/graph", timeout=5) as response:
        return json.loads(response.read())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--relay", choices=["cloudflare", "local"], default="cloudflare")
    parser.add_argument("--control-plane-port", type=int, default=19500)
    parser.add_argument("--output", default=str(HERE / "results" / "smoke"))
    arguments = parser.parse_args()
    output = Path(arguments.output)
    output.mkdir(parents=True, exist_ok=True)

    if arguments.relay == "cloudflare":
        host = os.environ["CLOUDFLARE_MOQ_DRAFT_16_URL"].strip().removeprefix("https://").strip("/")
        relay_url = f"https://{host}/{os.environ['CLOUDFLARE_MOQ_PUB_SUB_TOKEN']}"
        accept_any = []
    else:
        relay_url = "https://localhost:4443/local"
        accept_any = ["--accept-any-certificate"]

    run_id = secrets.token_hex(3)
    runtime_name = f"moq-smoke-{run_id}"
    handoff = Path(tempfile.mkdtemp(prefix="moq-gateway-handoff-"))
    (handoff / "relay.json").write_text(
        json.dumps({"relayPublishUrl": relay_url, "namespacePrefix": "example/spike"})
    )
    environment = dict(os.environ)
    environment.pop("STREAMLIB_MESH_TRANSPORT", None)
    environment.update(
        {
            "STREAMLIB_MOQ_GATEWAY_HANDOFF_DIR": str(handoff),
            "STREAMLIB_MESH_NAME": "moqspikesmoke",
            "STREAMLIB_MESH_MULTICAST_DISCOVERY": "0",
            "PYTHONPATH": str(HERE),
            "MOQ_BENCH_RATES": "50",
            "MOQ_BENCH_STEP_SECONDS": "600",
            "MOQ_BENCH_START_DELAY_SECONDS": "0",
        }
    )
    if arguments.relay == "local":
        environment["STREAMLIB_MESH_MOQ_DANGER_ACCEPT_ANY_CERTIFICATE"] = "1"
    node_log_path = output / f"node-{arguments.relay}.log"
    node_log = open(node_log_path, "w")
    node = subprocess.Popen(
        [sys.executable, str(HERE / "moq_gateway_bench_node.py"), "--role", "source",
         "--workload", "smoke", "--runtime-name", runtime_name,
         "--control-plane-port", str(arguments.control_plane_port)],
        env=environment, cwd=HERE, stdout=node_log, stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    report = {"relay": arguments.relay, "runtime_name": runtime_name, "tracks": {}}
    try:
        namespace = ""
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline and not namespace:
            time.sleep(1)
            try:
                namespace = the_graph(arguments.control_plane_port).get("moq_gateway", {}).get("namespace", "")
            except OSError:
                continue
        if not namespace:
            raise SystemExit("the gateway never announced a namespace; see " + str(node_log_path))
        report["namespace"] = namespace
        keys = {track: secrets.token_bytes(16) for track in TRACKS}
        (handoff / f"{runtime_name}.json").write_text(
            json.dumps(
                {
                    "paused": False,
                    "contentKeys": {
                        f"{namespace}/{track}": [
                            {"epoch": 1, "keyBase64Url": base64.urlsafe_b64encode(bytes(16)).decode().rstrip("=")},
                            {"epoch": 3, "keyBase64Url": base64.urlsafe_b64encode(key).decode().rstrip("=")},
                        ]
                        for track, key in keys.items()
                    },
                }
            )
        )
        time.sleep(3)

        subscriber_environment = dict(os.environ)
        subscriber_environment["MOQ_TEST_RELAY_URL"] = relay_url
        graph_while_subscribed = {}

        def sample_the_graph_midway() -> None:
            time.sleep(6)
            try:
                graph_while_subscribed.update(the_graph(arguments.control_plane_port))
            except OSError as failure:
                graph_while_subscribed["error"] = str(failure)

        for track in TRACKS:
            sampler = threading.Thread(target=sample_the_graph_midway)
            sampler.start()
            completed = subprocess.run(
                [str(SUBSCRIBER), "--namespace", namespace, "--track", track,
                 "--key-base64url", base64.urlsafe_b64encode(keys[track]).decode().rstrip("="),
                 "--count", "90", "--timeout-seconds", "40", *accept_any],
                env=subscriber_environment, capture_output=True, text=True,
            )
            sampler.join()
            lines = [json.loads(line) for line in completed.stdout.splitlines() if line.startswith("{")]
            objects = [line for line in lines if "summary" not in line]
            summary = next((line for line in lines if "summary" in line), {})
            gateway_track = next(
                (entry for entry in graph_while_subscribed.get("moq_gateway", {}).get("tracks", [])
                 if entry["relay_track"] == track),
                None,
            )
            report["tracks"][track] = {
                "subscriber_exit": completed.returncode,
                "subscriber_stderr": scrub(completed.stderr)[-2000:],
                "summary": summary,
                "every_object_sealed": bool(objects) and all(o.get("sealed") for o in objects),
                "every_object_decrypted": bool(objects) and all(o.get("decrypted") for o in objects),
                "key_epochs_seen": sorted({o.get("key_epoch") for o in objects if "key_epoch" in o}),
                "first_objects": objects[:3],
                "graph_track_while_subscribed": gateway_track,
            }
        report["graph_moq_gateway_state"] = graph_while_subscribed.get("moq_gateway", {}).get("state")
    finally:
        try:
            os.killpg(node.pid, signal.SIGTERM)
            node.wait(timeout=20)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            os.killpg(node.pid, signal.SIGKILL)
        node_log.close()
        node_log_path.write_text(scrub(node_log_path.read_text(errors="replace")))
        for leftover in handoff.iterdir():
            leftover.unlink()
        handoff.rmdir()
    rendered = scrub(json.dumps(report, indent=1))
    (output / f"smoke-{arguments.relay}.json").write_text(rendered)
    print(rendered)


if __name__ == "__main__":
    main()
