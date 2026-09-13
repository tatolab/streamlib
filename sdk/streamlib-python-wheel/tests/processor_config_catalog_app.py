# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The processor catalog a running node serves, read over its own control plane.

Run as a real `python app.py`: four processors are added, each hosted in its own
helper process, and this app then reads `GET /api/registry` off itself — the
exact payload an agent gets before deciding which keys a processor takes. A
fifth class is imported and never added, which is what an agent discovering an
app's effects reads.
"""

import json
import os
import sys
import threading
import urllib.request

import streamlib
from streamlib._node_registry import live_nodes

import processor_config_catalog_probes as probes

READ_TIMEOUT_SECONDS = 30.0
GRAPH_READY_TIMEOUT_SECONDS = 90.0


def _this_processes_control_url() -> str:
    return next(node.control_url for node in live_nodes() if node.pid == os.getpid())


def main() -> None:
    runtime = streamlib.Runtime()
    runtime.host_control_plane()
    runtime.add(probes.TypedDictConfiguredProbe, config={"width": 320})
    runtime.add(probes.DataclassConfiguredProbe, config={"width": 640, "label": "left"})
    runtime.add(probes.ModelConfiguredProbe, config={"width": 1280})
    runtime.add(probes.UnconfiguredProbe)
    # `probes.ImportedButNeverAddedProbe` is deliberately not added: importing
    # the module is what put it in the catalog.

    def read_the_catalog_this_node_serves() -> None:
        try:
            runtime.wait_until_every_processor_is_running(
                timeout=GRAPH_READY_TIMEOUT_SECONDS
            )
            registry_url = f"{_this_processes_control_url()}/api/registry"
            # A loopback URL this app minted, read back off itself.
            with urllib.request.urlopen(registry_url, timeout=READ_TIMEOUT_SECONDS) as response:
                served = json.load(response)
            catalog = {
                entry["processor_class_import_path"]: entry
                for entry in served["processors"]
                if entry["processor_class_import_path"].startswith(
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
