# Capability-extension fixtures

Three test-only distributions, each a directory carrying a package and the
`.dist-info` pip would have written for it. They are put on `sys.path` rather
than installed: the raising variant would fail every `Runtime()` in the suite
if it lived in the shared venv, and the registering one would put an entry in
every other test's graph JSON.

`importlib.metadata` discovers a distribution by finding its `<name>-<version>.dist-info`
directory on `sys.path` — the same directory pip writes at install — so what the
engine reads here is pip's registry format and not a file scan of its own.
