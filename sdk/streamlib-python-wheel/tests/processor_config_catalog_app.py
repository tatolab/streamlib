# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The processor catalog a running node serves, read over its own control plane.

Run as its own `python <script>.py` process: four processors are added, each
hosted in its own helper process, and this app then reads `GET /api/registry`
off itself — the exact payload an agent gets before deciding which keys a
processor takes.
"""

import json
import sys
import threading

import tatolab.runtime
from tatolab.stream import StreamBuilder, compile_stream_to_graph, stream
from tatolab.runtime._control_plane_client import _request_over_the_local_api_socket
from this_processes_node_registry_entry import this_processes_local_api_socket

import processor_config_catalog_probes as probes

READ_TIMEOUT_SECONDS = 30.0
GRAPH_READY_TIMEOUT_SECONDS = 90.0


@stream
def four_probes_each_configured_its_own_way(stream_builder: StreamBuilder) -> None:
    """Probes configured by a TypedDict, a dataclass and a model, beside one taking none."""
    stream_builder.add(probes.TypedDictConfiguredProbe, config={"width": 320})
    stream_builder.add(probes.DataclassConfiguredProbe, config={"width": 640, "label": "left"})
    stream_builder.add(probes.ModelConfiguredProbe, config={"width": 1280})
    stream_builder.add(probes.UnconfiguredProbe)


def main() -> None:
    graph = compile_stream_to_graph(four_probes_each_configured_its_own_way)
    runtime = tatolab.runtime.Runtime()
    runtime.load(graph)
    runtime.host_control_plane()

    def read_the_catalog_this_node_serves() -> None:
        try:
            runtime.wait_until_every_node_is_running(
                timeout=GRAPH_READY_TIMEOUT_SECONDS
            )
            answered = _request_over_the_local_api_socket(
                this_processes_local_api_socket(),
                method="GET",
                path="/api/registry",
                timeout_seconds=READ_TIMEOUT_SECONDS,
            )
            if answered.status != 200:
                raise RuntimeError(f"GET /api/registry answered {answered.status}")
            served = json.loads(answered.body)
            catalog = {
                entry["type"]: entry
                for entry in served["nodes"]
                if entry["type"].startswith(
                    "processor_config_catalog_probes:"
                )
            }
            print(f"MARKER:CATALOG {json.dumps(catalog)}", flush=True)
        except Exception as read_failure:
            # Said rather than swallowed: this runs on a daemon thread, where an
            # unhandled raise leaves the test waiting on a marker that never comes.
            print(f"MARKER:CATALOG_FAILED {read_failure!r}", flush=True)
        finally:
            runtime.shutdown()

    threading.Thread(target=read_the_catalog_this_node_serves, daemon=True).start()
    runtime.run()
    print("MARKER:CLEAN_EXIT", flush=True)


if __name__ == "__main__":
    main()
    sys.exit(0)
