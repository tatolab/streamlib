# Changelog

## [0.12.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.11.0...streamlib-webrtc-v0.12.0) (2026-10-08)


### ⚠ BREAKING CHANGES

* **engine:** processor interpreters start from the stream's own venv and borrow the runtime through the lend ([#2689](https://github.com/tatolab/streamlib/issues/2689))

### Features

* **engine:** processor interpreters start from the stream's own venv and borrow the runtime through the lend ([#2689](https://github.com/tatolab/streamlib/issues/2689)) ([3cfe910](https://github.com/tatolab/streamlib/commit/3cfe910887411a7483540eb84eb7174c1cd1b4cd))

## [0.11.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.10.0...streamlib-webrtc-v0.11.0) (2026-10-07)


### ⚠ BREAKING CHANGES

* **sdk:** `tatolab-stream` installs, type-checks and is tested with no runtime ([#2686](https://github.com/tatolab/streamlib/issues/2686))

### Features

* **sdk:** `tatolab-stream` installs, type-checks and is tested with no runtime ([#2686](https://github.com/tatolab/streamlib/issues/2686)) ([bb72729](https://github.com/tatolab/streamlib/commit/bb72729ba379584ab28f1db0f8e07fabae465ed0))

## [0.10.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.9.0...streamlib-webrtc-v0.10.0) (2026-10-07)


### ⚠ BREAKING CHANGES

* **sdk:** `from tatolab.stream import Stream`, `input` and `output` raise ImportError. Write `StreamBuilder`, `@node.input(...)` and `@node.output(...)`.

### Features

* **sdk:** the builder is `StreamBuilder` and ports are `[@node](https://github.com/node).input` / `[@node](https://github.com/node).output` ([#2682](https://github.com/tatolab/streamlib/issues/2682)) ([fd66702](https://github.com/tatolab/streamlib/commit/fd6670245d5df0d63bb697d13890fb3919c1ccff))

## [0.9.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.8.0...streamlib-webrtc-v0.9.0) (2026-10-06)


### ⚠ BREAKING CHANGES

* **sdk:** `streamlib` becomes `tatolab.stream` and `tatolab.runtime` ([#2673](https://github.com/tatolab/streamlib/issues/2673))

### Features

* **sdk:** `streamlib` becomes `tatolab.stream` and `tatolab.runtime` ([#2673](https://github.com/tatolab/streamlib/issues/2673)) ([d7f6807](https://github.com/tatolab/streamlib/commit/d7f680741e2f28d73c254ed80a4e09d5d705a553))

## [0.8.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.7.0...streamlib-webrtc-v0.8.0) (2026-10-06)


### ⚠ BREAKING CHANGES

* **sdk:** the control plane's TCP port, --host/--port/--url and the bearer gate are gone ([#2663](https://github.com/tatolab/streamlib/issues/2663))

### Features

* **sdk:** the control plane's TCP port, --host/--port/--url and the bearer gate are gone ([#2663](https://github.com/tatolab/streamlib/issues/2663)) ([ca3dfeb](https://github.com/tatolab/streamlib/commit/ca3dfeba03f158593cba0607c9dee5cafd1bcbdb))

## [0.7.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.6.0...streamlib-webrtc-v0.7.0) (2026-10-05)


### ⚠ BREAKING CHANGES

* **engine:** a runtime opens no Zenoh session — the mesh, its flags and the zenoh dependency are gone ([#2645](https://github.com/tatolab/streamlib/issues/2645))

### Features

* **engine:** a runtime opens no Zenoh session — the mesh, its flags and the zenoh dependency are gone ([#2645](https://github.com/tatolab/streamlib/issues/2645)) ([2a15a66](https://github.com/tatolab/streamlib/commit/2a15a66279bc7d1e5bb6dde351b25b0131541487))

## [0.6.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.5.0...streamlib-webrtc-v0.6.0) (2026-10-05)


### ⚠ BREAKING CHANGES

* **engine:** every link has both ends on one runtime — links between runtimes are gone ([#2643](https://github.com/tatolab/streamlib/issues/2643))

### Features

* **engine:** every link has both ends on one runtime — links between runtimes are gone ([#2643](https://github.com/tatolab/streamlib/issues/2643)) ([d36ce23](https://github.com/tatolab/streamlib/commit/d36ce23d53079cbddad1d41f2faeaf8763fee6ab))

## [0.5.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.4.0...streamlib-webrtc-v0.5.0) (2026-10-04)


### ⚠ BREAKING CHANGES

* **engine:** the graph is one shape — the snapshot a runtime loads is what graph renders ([#2614](https://github.com/tatolab/streamlib/issues/2614))

### Features

* **engine:** the graph is one shape — the snapshot a runtime loads is what graph renders ([#2614](https://github.com/tatolab/streamlib/issues/2614)) ([66a0675](https://github.com/tatolab/streamlib/commit/66a0675e34376c23afe1322a4f61bc22256ef553))

## [0.4.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.3.3...streamlib-webrtc-v0.4.0) (2026-10-03)


### ⚠ BREAKING CHANGES

* **sdk:** `streamlib.processor` is gone; decorate node classes with `streamlib.node`. A scaffolded app's node modules live under `nodes/`.

### Features

* **sdk:** `[@node](https://github.com/node)` declares a node where `[@processor](https://github.com/processor)` did ([#2612](https://github.com/tatolab/streamlib/issues/2612)) ([8f411f2](https://github.com/tatolab/streamlib/commit/8f411f2f3705f1678c01a8facd7b6ce298d1749d))

## [0.3.3](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.3.2...streamlib-webrtc-v0.3.3) (2026-10-01)


### Features

* **packages:** the MoQ and WebRTC extension wheels build, test and release on macOS, on the engine's clock ([#2555](https://github.com/tatolab/streamlib/issues/2555)) ([417d368](https://github.com/tatolab/streamlib/commit/417d368b54568aca782a8995aa6700664babb0d1))

## [0.3.2](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.3.1...streamlib-webrtc-v0.3.2) (2026-09-20)


### Features

* **engine:** show each remote link's stamp clock, and stop an Mp4Sink track from another clock ([#2382](https://github.com/tatolab/streamlib/issues/2382)) ([872d8e4](https://github.com/tatolab/streamlib/commit/872d8e49f8996fc8af21f7450cad88630b48fea2))

## [0.3.1](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.3.0...streamlib-webrtc-v0.3.1) (2026-09-19)


### Features

* **wheel:** wire a remote output from Python and MCP, into helper-placed processors too ([#2342](https://github.com/tatolab/streamlib/issues/2342)) ([a8cf4ad](https://github.com/tatolab/streamlib/commit/a8cf4adb6877f513bffaef2012774e03bc7e8e45))

## [0.3.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.2.2...streamlib-webrtc-v0.3.0) (2026-09-18)


### ⚠ BREAKING CHANGES

* **engine:** every runtime carries a stable name it owns — hashed default, set by constructor, environment or CLI ([#2329](https://github.com/tatolab/streamlib/issues/2329))

### Features

* **engine:** every runtime carries a stable name it owns — hashed default, set by constructor, environment or CLI ([#2329](https://github.com/tatolab/streamlib/issues/2329)) ([d8bd91e](https://github.com/tatolab/streamlib/commit/d8bd91e7414e5f1abc855827d059151693077091))

## [0.2.2](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.2.1...streamlib-webrtc-v0.2.2) (2026-09-15)


### Features

* **engine:** size every channel for a late consumer of any delivery profile and raise the link caps to 32 + tap and 256 ([#2301](https://github.com/tatolab/streamlib/issues/2301)) ([bf2b1e0](https://github.com/tatolab/streamlib/commit/bf2b1e018412eccdfea312c5be2be10645b4eb94))

## [0.2.1](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.2.0...streamlib-webrtc-v0.2.1) (2026-09-15)


### Features

* **engine:** resolve one runtime directory and run iceoryx2 in an engine-owned domain per OS user ([#2294](https://github.com/tatolab/streamlib/issues/2294)) ([bc87317](https://github.com/tatolab/streamlib/commit/bc87317b4c7c79a495c4b6c598d9a04795c95785))

## [0.2.0](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.1.1...streamlib-webrtc-v0.2.0) (2026-09-11)


### ⚠ BREAKING CHANGES

* **wheel:** keyword-argument configuration is deleted. A processor's `__init__` takes one `config` parameter annotated with its config class, or nothing beyond `self`; any other signature is refused at decoration.

### Features

* **wheel:** a Python processor's config is one class named by its __init__ annotation ([#2226](https://github.com/tatolab/streamlib/issues/2226)) ([9033ca9](https://github.com/tatolab/streamlib/commit/9033ca9b06fb292ae63f8a7d8d4a26a57d04e351))

## [0.1.1](https://github.com/tatolab/streamlib/compare/streamlib-webrtc-v0.1.0...streamlib-webrtc-v0.1.1) (2026-09-05)


### Features

* **extension:** streamlib-webrtc — a standalone maturin wheel with WhipPublisher and WhepPlayer on the mined clients and h264_rtp.rs ([#2156](https://github.com/tatolab/streamlib/issues/2156)) ([80e652d](https://github.com/tatolab/streamlib/commit/80e652da9f1dc48fe50f79709740c7fe2a734340))
