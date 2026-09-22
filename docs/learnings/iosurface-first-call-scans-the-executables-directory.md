# The first IOSurface call in a process scans the executable's directory

## Symptom

The first `IOSurfaceCreate` (or any first IOSurface call) in a `cargo test`
binary takes about three seconds, while the same call in a small standalone
binary takes about ten milliseconds. Every later call is fast. A sampled stack
shows the time inside `-[NSBundle objectForInfoDictionaryKey:]` →
`_CFBundleReadDirectory`, under `_ioSurfaceConnectInternal`.

## Constraint

IOSurface's first connection reads a key from the main bundle's info
dictionary. An unbundled executable's main bundle is the directory it sits
in, and CFBundle lists that whole directory to find resources.
`target/debug/deps/`, where cargo puts test binaries, holds thousands of
files, so the listing is the three seconds. It happens once per process.

## Fix pattern

Nothing to fix in the engine. A shipped helper runs as the Python
interpreter, whose directory is small. In tests:

- budget for it — any wait on a helper's first IOSurface event must allow
  several seconds;
- don't read it as a regression when a test that creates its first
  IOSurface is slow and its siblings are not;
- to time IOSurface itself, run from a binary whose directory is small.
