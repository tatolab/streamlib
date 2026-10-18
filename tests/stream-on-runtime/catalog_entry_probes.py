# Copyright (c) 2025 Jonathan Fontanez
# SPDX-License-Identifier: BUSL-1.1

"""The nodes `test_catalog_entries.py` drives: two sources and a reader.

In its own module because a helper process imports each class by its import
path, and a class declared inside a pytest module would have the child import
the test suite.
"""

import json

from tatolab.stream import RuntimeContextLimitedAccess, log, node
from tatolab.stream.catalog import BinaryEntry, JsonEntry, VideoEntry
from tatolab.stream.msgpack import pack

DECLARED_ENTRY = BinaryEntry(mode="snapshot", mime="application/msgpack")
REPLACEMENT_ENTRY = JsonEntry(mode="stream")


@node(execution="continuous", interval_ms=50)
class KeepsItsDeclaredCatalogEntry:
    """Writes a record every tick and never touches its port's entry."""

    @node.output(description="Records", catalog=DECLARED_ENTRY)
    def records(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        ctx.outputs.write("records", pack({"kept": True}))


@node(execution="continuous", interval_ms=50)
class ReplacesItsCatalogEntryOnItsFirstTick:
    """Replaces its port's entry on its first tick, after trying one hang refuses.

    Reports `MARKER:CATALOG_ENTRY_REFUSED <message>` and then
    `MARKER:CATALOG_ENTRY_REPLACED`, so a reader of `graph` knows the
    replacement has landed before it looks.
    """

    def __init__(self) -> None:
        self.replaced = False

    @node.output(description="Records", catalog=DECLARED_ENTRY)
    def records(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if not self.replaced:
            self.replaced = True
            try:
                ctx.outputs.set_catalog_entry("records", VideoEntry(codec="avc1.zz"))
            except Exception as refusal:
                log.info("MARKER:CATALOG_ENTRY_REFUSED " + json.dumps(str(refusal)))
            ctx.outputs.set_catalog_entry("records", REPLACEMENT_ENTRY)
            log.info("MARKER:CATALOG_ENTRY_REPLACED")
        ctx.outputs.write("records", pack({"replaced": True}))


@node
class ReportsItsUpstreamsCatalogEntry:
    """Reports the entry its one upstream port carries, every message, as
    `MARKER:UPSTREAM_CATALOG_ENTRY <json>`."""

    @node.input(delivery_profile="newest")
    def records_from_upstream(self) -> None: ...

    def process(self, ctx: RuntimeContextLimitedAccess) -> None:
        if ctx.inputs.read("records_from_upstream") is None:
            return
        log.info(
            "MARKER:UPSTREAM_CATALOG_ENTRY "
            + json.dumps(ctx.inputs.inbound_link_catalog_entry("records_from_upstream"))
        )
