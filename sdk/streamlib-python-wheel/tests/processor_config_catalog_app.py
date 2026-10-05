# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The processor catalog a running node serves, read over its own control plane.

Run as its own `python <script>.py` process: four processors are added, each
hosted in its own helper process, and this app then reads `GET /api/registry`
off itself — the exact payload an agent gets before deciding which keys a
processor takes. A fifth class is imported and never added, which is what an
agent discovering an app's effects reads.
"""

import json
import sys
import threading
import urllib.request

import streamlib
from streamlib import Stream, compile_stream_to_graph, stream
from this_processes_node_registry_entry import this_processes_control_url

import processor_config_catalog_probes as probes

READ_TIMEOUT_SECONDS = 30.0
GRAPH_READY_TIMEOUT_SECONDS = 90.0


@stream
def four_probes_each_configured_its_own_way(stream: Stream) -> None:
    """Probes configured by a TypedDict, a dataclass and a model, beside one taking none."""
    stream.add(probes.TypedDictConfiguredProbe, config={"width": 320})
    stream.add(probes.DataclassConfiguredProbe, config={"width": 640, "label": "left"})
    stream.add(probes.ModelConfiguredProbe, config={"width": 1280})
    stream.add(probes.UnconfiguredProbe)
    # `probes.ImportedButNeverAddedProbe` is deliberately not added: importing
    # the module is what put it in the catalog.


def main() -> None:
    graph = compile_stream_to_graph(four_probes_each_configured_its_own_way)
    runtime = streamlib.Runtime()
    runtime.load(graph)
    runtime.host_control_plane()

    def read_the_catalog_this_node_serves() -> None:
        try:
            runtime.wait_until_every_processor_is_running(
                timeout=GRAPH_READY_TIMEOUT_SECONDS
            )
            registry_url = f"{this_processes_control_url()}/api/registry"
            # A loopback URL this app minted, read back off itself.
            with urllib.request.urlopen(registry_url, timeout=READ_TIMEOUT_SECONDS) as response:
                served = json.load(response)
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
